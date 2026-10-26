//! Column widths the way CSS grid shares them out: fixed columns take their width, flexible
//! columns split what is left by their fraction, and none drops below its minimum. The table's
//! header and rows use the same columns, so they always line up.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Column {
    Fixed(f32),
    /// `minmax(min, <fr>fr)`.
    Flex {
        min: f32,
        fr: f32,
    },
}

/// The left edge and width of each column across `width`, with `gap` between columns.
pub fn columns(width: f32, gap: f32, columns: &[Column]) -> Vec<(f32, f32)> {
    let gaps = gap * columns.len().saturating_sub(1) as f32;
    let fixed: f32 = columns
        .iter()
        .map(|c| match c {
            Column::Fixed(w) => *w,
            Column::Flex { .. } => 0.0,
        })
        .sum();
    let mut widths: Vec<Option<f32>> = columns
        .iter()
        .map(|c| match c {
            Column::Fixed(w) => Some(*w),
            Column::Flex { .. } => None,
        })
        .collect();
    let mut free = (width - gaps - fixed).max(0.0);
    // Freeze any flexible column whose share falls under its minimum, then share again.
    loop {
        let fr_total: f32 = columns
            .iter()
            .zip(&widths)
            .filter_map(|(c, w)| match (c, w) {
                (Column::Flex { fr, .. }, None) => Some(*fr),
                _ => None,
            })
            .sum();
        if fr_total <= 0.0 {
            break;
        }
        let mut froze = false;
        for (column, slot) in columns.iter().zip(widths.iter_mut()) {
            if let (Column::Flex { min, fr }, None) = (column, &slot) {
                if free * fr / fr_total < *min {
                    *slot = Some(*min);
                    free = (free - min).max(0.0);
                    froze = true;
                }
            }
        }
        if !froze {
            for (column, slot) in columns.iter().zip(widths.iter_mut()) {
                if let (Column::Flex { fr, .. }, None) = (column, &slot) {
                    *slot = Some(free * fr / fr_total);
                }
            }
            break;
        }
    }
    let mut x = 0.0;
    widths
        .into_iter()
        .map(|w| {
            let w = w.unwrap_or(0.0);
            let column = (x, w);
            x += w + gap;
            column
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flexible_columns_share_what_fixed_columns_leave() {
        let cols = columns(
            100.0,
            0.0,
            &[
                Column::Fixed(40.0),
                Column::Flex { min: 0.0, fr: 1.0 },
                Column::Flex { min: 0.0, fr: 2.0 },
            ],
        );
        assert_eq!(cols, vec![(0.0, 40.0), (40.0, 20.0), (60.0, 40.0)]);
    }

    #[test]
    fn a_column_never_shrinks_below_its_minimum() {
        let cols = columns(
            100.0,
            10.0,
            &[
                Column::Flex { min: 60.0, fr: 1.0 },
                Column::Flex { min: 0.0, fr: 3.0 },
            ],
        );
        assert_eq!(cols, vec![(0.0, 60.0), (70.0, 30.0)]);
    }
}
