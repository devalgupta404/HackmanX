// The dirty-block tracker's kernel half (implementation I5.3). A tracepoint on block request
// issue marks every data-changing write to the tracked disk in a bitmap: one bit per 4 KiB block,
// double-buffered by generation so writes during a drain land in the other buffer. The bitmap is a
// per-CPU map, so each CPU writes its own copy with no atomics (clang here is too old to emit BPF
// atomic ops); userspace ORs the copies together on drain.
//
// Self-contained: it declares the handful of helpers and map macros it needs, so compiling it needs
// only clang, not the libbpf headers. The tracepoint argument layout is the kernel's stable trace
// format for block/block_rq_issue.

typedef unsigned char __u8;
typedef unsigned int __u32;
typedef unsigned long long __u64;

#define SEC(name) __attribute__((section(name), used))
#define __uint(name, val) int (*name)[val]
#define __type(name, val) typeof(val) *name

#define BPF_MAP_TYPE_ARRAY 2
#define BPF_MAP_TYPE_PERCPU_ARRAY 6

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;

// One bit per 4 KiB block; words are 64 bits.
#define TRACK_BLOCK 4096ULL
#define BITS_PER_WORD 64ULL
// The most 64-block words one event may mark before it is treated as too large (64 words = 16 MiB).
// A larger write bumps the overflow counter instead, which userspace reads as "tracking broke". The
// bound keeps the kernel verifier's loop small: one map update per touched word, not per block.
#define MAX_WORDS 64

enum counter {
    COUNTER_WRITES = 0,
    COUNTER_OVERFLOW = 1,
    COUNTER_COUNT = 2,
};

struct config {
    __u32 dev;            // the tracked disk's kernel dev_t
    __u32 generation;     // writes land in buffer[generation & 1]
    __u64 words_per_gen;  // 64-bit words in one generation's bitmap
};

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct config);
} config_map SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u64);
} bitmap_map SEC(".maps");  // resized to words_per_gen * 2 by userspace before load

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, COUNTER_COUNT);
    __type(key, __u32);
    __type(value, __u64);
} counters_map SEC(".maps");

// block/block_rq_issue trace entry: 8 bytes of common header, then the event fields at their
// stable offsets (dev at 8, sector at 16, nr_sector at 24, rwbs at 32).
struct rq_issue_ctx {
    __u64 common;
    __u32 dev;
    __u32 _pad;
    __u64 sector;
    __u32 nr_sector;
    __u32 bytes;
    char rwbs[8];
};

static __attribute__((always_inline)) void bump(__u32 which) {
    __u64 *c = bpf_map_lookup_elem(&counters_map, &which);
    if (c)
        *c += 1;
}

// Sets, in one generation's buffer, the bits for every 4 KiB block in [first, last]. Works a word
// at a time: for each touched 64-block word it ORs in a mask of the covered bits, so the loop runs
// once per word (usually one or two) rather than once per block.
static __attribute__((always_inline)) void mark_in_gen(__u32 generation, __u64 first, __u64 last,
                                                       __u64 words_per_gen) {
    __u64 base = (__u64)(generation & 1) * words_per_gen;
    __u64 first_word = first / BITS_PER_WORD;
    __u64 last_word = last / BITS_PER_WORD;
    for (__u32 i = 0; i < MAX_WORDS; i++) {
        __u64 w = first_word + i;
        if (w > last_word)
            break;
        __u64 lo = (w == first_word) ? (first & 63) : 0;   // first covered bit in this word
        __u64 hi = (w == last_word) ? (last & 63) : 63;    // last covered bit in this word
        __u64 mask;
        if (hi >= 63)
            mask = ~0ULL << lo;
        else
            mask = ((1ULL << (hi + 1)) - 1) & (~0ULL << lo);
        __u32 key = (__u32)(base + w);
        __u64 *word = bpf_map_lookup_elem(&bitmap_map, &key);
        if (word)
            *word |= mask;
    }
}

SEC("tp/block/block_rq_issue")
int on_rq_issue(struct rq_issue_ctx *ctx) {
    __u32 key = 0;
    struct config *cfg = bpf_map_lookup_elem(&config_map, &key);
    if (!cfg)
        return 0;
    if (ctx->dev != cfg->dev)
        return 0;

    // A data-changing op: write, discard, or write-zeroes. Reads and bare flushes are skipped.
    int changes = 0;
    for (int i = 0; i < 8; i++) {
        char ch = ctx->rwbs[i];
        if (ch == 'W' || ch == 'D' || ch == 'Z')
            changes = 1;
    }
    if (!changes)
        return 0;
    if (ctx->nr_sector == 0)
        return 0;

    __u64 start = ctx->sector * 512ULL;
    __u64 end = start + (__u64)ctx->nr_sector * 512ULL; // exclusive
    __u64 first = start / TRACK_BLOCK;
    __u64 last = (end - 1) / TRACK_BLOCK;

    __u64 words_per_gen = cfg->words_per_gen;
    if (last >= words_per_gen * BITS_PER_WORD) {
        bump(COUNTER_OVERFLOW); // past the disk's bitmap: tracking can't be trusted
        return 0;
    }
    if ((last / BITS_PER_WORD) - (first / BITS_PER_WORD) + 1 > MAX_WORDS) {
        bump(COUNTER_OVERFLOW); // too large to mark in one bounded pass
        return 0;
    }

    __u32 generation = cfg->generation;
    mark_in_gen(generation, first, last, words_per_gen);
    // If a flip raced us, mark the new generation too, so the write is in whichever one the sender
    // drains (the overlapping-window rule, M5).
    struct config *again = bpf_map_lookup_elem(&config_map, &key);
    if (again && again->generation != generation)
        mark_in_gen(again->generation, first, last, words_per_gen);

    bump(COUNTER_WRITES);
    return 0;
}

char _license[] SEC("license") = "GPL";
