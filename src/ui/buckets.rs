//! Maps frames onto chart columns. Buckets follow the absolute frame sequence number, so a finished column keeps its frames while the window scrolls.

use std::ops::Range;

/// Lays `width` columns over the frames in `seqs` (oldest first, at most `window_frames` long) and returns, for each column left to right, the run of indexes into `seqs` it covers. `None` marks a column with no data yet.
pub fn column_ranges(
    seqs: &[u64],
    width: usize,
    window_frames: usize,
) -> Vec<Option<Range<usize>>> {
    if width == 0 || window_frames == 0 || seqs.is_empty() {
        return vec![None; width];
    }
    let cells = if window_frames >= width {
        bucketed(seqs, width, window_frames)
    } else {
        spread(seqs, width, width.div_ceil(window_frames))
    };
    right_align(cells, width)
}

/// Groups consecutive frames into `width` buckets per window by the absolute formula `seq * width / window_frames`. The same frame always lands in the same bucket, and the buckets fill the full width even when the window is not a multiple of it.
fn bucketed(seqs: &[u64], width: usize, window_frames: usize) -> Vec<Option<Range<usize>>> {
    let mut runs: Vec<(u128, Range<usize>)> = Vec::new();
    for (index, seq) in seqs.iter().enumerate() {
        let bucket = u128::from(*seq) * width as u128 / window_frames as u128;
        match runs.last_mut() {
            Some((id, range)) if *id == bucket => range.end = index + 1,
            _ => runs.push((bucket, index..index + 1)),
        }
    }
    runs.into_iter().map(|(_, range)| Some(range)).collect()
}

/// Gives every frame `columns_per_frame` consecutive columns, keeping only the last columns when they do not all fit.
fn spread(seqs: &[u64], width: usize, columns_per_frame: usize) -> Vec<Option<Range<usize>>> {
    let total = seqs.len().saturating_mul(columns_per_frame);
    let shown = total.min(width);
    (total - shown..total)
        .map(|virtual_column| {
            let frame = virtual_column / columns_per_frame;
            Some(frame..frame + 1)
        })
        .collect()
}

/// Keeps the newest `width` cells and pads the left with `None` so the newest cell sits in the last column.
fn right_align(cells: Vec<Option<Range<usize>>>, width: usize) -> Vec<Option<Range<usize>>> {
    let kept = cells.len().min(width);
    let start = cells.len() - kept;
    let mut columns = vec![None; width - kept];
    columns.extend(cells.into_iter().skip(start));
    columns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covered(seqs: &[u64], columns: &[Option<Range<usize>>]) -> Vec<Vec<u64>> {
        columns
            .iter()
            .map(|column| {
                column
                    .as_ref()
                    .map_or_else(Vec::new, |r| seqs[r.clone()].to_vec())
            })
            .collect()
    }

    #[test]
    fn buckets_follow_sequence_and_right_align() {
        let seqs: Vec<u64> = (0..12).collect();
        let columns = column_ranges(&seqs, 4, 20);
        assert_eq!(columns, vec![None, Some(0..5), Some(5..10), Some(10..12),]);
    }

    #[test]
    fn the_whole_width_is_used_even_when_the_window_is_not_a_multiple() {
        for (width, window) in [(59usize, 60usize), (100, 101), (120, 300), (7, 50)] {
            let seqs: Vec<u64> = (1000..1000 + window as u64).collect();
            let columns = column_ranges(&seqs, width, window);
            assert!(columns.iter().all(Option::is_some), "{width}x{window}");
            assert_eq!(columns.len(), width);
        }
    }

    #[test]
    fn oldest_buckets_are_dropped_when_they_do_not_fit() {
        let seqs: Vec<u64> = (5..25).collect();
        let columns = column_ranges(&seqs, 2, 20);
        assert_eq!(columns, vec![Some(5..15), Some(15..20)]);
    }

    #[test]
    fn complete_columns_survive_a_one_frame_scroll() {
        let before: Vec<u64> = (0..300).collect();
        let after: Vec<u64> = (1..301).collect();
        let old = covered(&before, &column_ranges(&before, 60, 300));
        let new = covered(&after, &column_ranges(&after, 60, 300));
        for column in 0..59 {
            assert_eq!(new[column], old[column + 1], "column {column}");
        }
        assert_eq!(new[59], vec![300]);
        assert_eq!(old[0], vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn few_frames_expand_across_several_columns() {
        let seqs: Vec<u64> = (0..3).collect();
        let columns = column_ranges(&seqs, 100, 60);
        assert_eq!(columns.len(), 100);
        assert!(columns[..94].iter().all(Option::is_none));
        assert_eq!(
            columns[94..].to_vec(),
            vec![
                Some(0..1),
                Some(0..1),
                Some(1..2),
                Some(1..2),
                Some(2..3),
                Some(2..3),
            ]
        );
    }

    #[test]
    fn full_history_expands_two_columns_per_frame() {
        let seqs: Vec<u64> = (0..60).collect();
        let columns = column_ranges(&seqs, 100, 60);
        assert_eq!(columns[0], Some(10..11));
        assert_eq!(columns[98], Some(59..60));
        assert_eq!(columns[99], Some(59..60));
    }

    #[test]
    fn partially_cut_oldest_frame_fills_the_leftmost_column() {
        let seqs = [7, 7];
        let columns = column_ranges(&seqs, 5, 2);
        assert_eq!(
            columns,
            vec![Some(0..1), Some(0..1), Some(1..2), Some(1..2), Some(1..2),]
        );
    }

    #[test]
    fn repeated_and_jumping_sequences_group_by_bucket_without_panicking() {
        let seqs = [5, 5, 5, 9, 2, 2];
        let columns = column_ranges(&seqs, 3, 6);
        assert_eq!(columns, vec![Some(0..3), Some(3..4), Some(4..6)]);
    }

    #[test]
    fn degenerate_inputs_give_empty_columns() {
        assert!(column_ranges(&[1, 2, 3], 0, 10).is_empty());
        assert_eq!(column_ranges(&[1, 2, 3], 3, 0), vec![None, None, None]);
        assert_eq!(column_ranges(&[], 4, 8), vec![None, None, None, None]);
    }
}
