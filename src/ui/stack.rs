//! Sub-cell layout for stacked bars drawn with eighth-block glyphs.

/// One coloured part of a bar; `tag` is the caller's colour key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub value: f64,
    pub tag: usize,
}

/// One terminal cell: a glyph drawn with an optional foreground and background tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub glyph: char,
    pub fg: Option<usize>,
    pub bg: Option<usize>,
}

/// Partial glyphs for horizontal bars; the index is the eighths filled from the left edge.
pub const LEFT_EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

/// Partial glyphs for vertical bars; the index is the eighths filled from the bottom edge.
pub const LOWER_EIGHTHS: [char; 8] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇'];

const SUB_STEPS: usize = 8;
const FULL_BLOCK: char = '█';
const EMPTY: Cell = Cell {
    glyph: ' ',
    fg: None,
    bg: None,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
    tag: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    tag: Option<usize>,
    steps: usize,
}

/// Resolves segments (left to right) into `width` cells. `total` is the value that fills the whole bar.
pub fn resolve_horizontal(segments: &[Segment], total: f64, width: usize) -> Vec<Cell> {
    resolve(segments, total, width, &LEFT_EIGHTHS)
}

/// Resolves segments (bottom to top) into `height` cells. `total` is the value that fills the whole bar.
pub fn resolve_vertical(segments: &[Segment], total: f64, height: usize) -> Vec<Cell> {
    resolve(segments, total, height, &LOWER_EIGHTHS)
}

fn resolve(segments: &[Segment], total: f64, cells: usize, eighths: &[char; 8]) -> Vec<Cell> {
    let spans = spans(segments, total, cells);
    (0..cells)
        .map(|cell| render(&dominant_runs(runs_in_cell(&spans, cell)), eighths))
        .collect()
}

/// Positions every non-empty segment on the sub-step grid. Boundaries are the rounded cumulative
/// sums, so rounding error never accumulates across segments.
fn spans(segments: &[Segment], total: f64, cells: usize) -> Vec<Span> {
    let valid = valid_segments(segments);
    if !is_usable_total(total) || valid.is_empty() {
        return Vec::new();
    }
    let largest = valid.iter().map(|&(_, value)| value).fold(0.0, f64::max);
    let normalized_sum = valid
        .iter()
        .fold(0.0, |acc, &(_, value)| acc + value / largest);
    let denominator = (total / largest).max(normalized_sum);
    let steps = cells as f64 * SUB_STEPS as f64;

    let mut spans = Vec::with_capacity(valid.len());
    let mut cumulative = 0.0;
    let mut previous = 0;
    for (tag, value) in valid {
        cumulative += value / largest;
        let boundary = boundary_at(cumulative, denominator, steps);
        if boundary > previous {
            spans.push(Span {
                start: previous,
                end: boundary,
                tag,
            });
            previous = boundary;
        }
    }
    spans
}

/// Maps a cumulative share of the bar onto the nearest sub-step. Saturating casts make NaN and
/// infinite inputs safe.
fn boundary_at(cumulative: f64, denominator: f64, steps: f64) -> usize {
    (cumulative / denominator * steps).round() as usize
}

/// Keeps only segments that can be drawn: finite and strictly positive.
fn valid_segments(segments: &[Segment]) -> Vec<(usize, f64)> {
    segments
        .iter()
        .filter(|segment| segment.value.is_finite() && segment.value > 0.0)
        .map(|segment| (segment.tag, segment.value))
        .collect()
}

fn is_usable_total(total: f64) -> bool {
    total.is_finite() && total > 0.0
}

/// Lists the coloured runs inside one cell from left (or bottom) to right (or top), with the
/// unfilled remainder as a trailing `None` run.
fn runs_in_cell(spans: &[Span], cell: usize) -> Vec<Run> {
    let low = cell * SUB_STEPS;
    let high = low + SUB_STEPS;
    let mut runs = Vec::new();
    let mut covered = 0;
    for span in spans {
        let overlap = span.end.min(high).saturating_sub(span.start.max(low));
        if overlap > 0 {
            push_merged(
                &mut runs,
                Run {
                    tag: Some(span.tag),
                    steps: overlap,
                },
            );
            covered += overlap;
        }
    }
    if covered < SUB_STEPS {
        push_merged(
            &mut runs,
            Run {
                tag: None,
                steps: SUB_STEPS - covered,
            },
        );
    }
    runs
}

/// Appends a run, folding it into the previous one when both share a tag.
fn push_merged(runs: &mut Vec<Run>, run: Run) {
    match runs.last_mut() {
        Some(last) if last.tag == run.tag => last.steps += run.steps,
        _ => runs.push(run),
    }
}

/// Reduces a cell to at most two runs: the two with the most sub-steps, ties going to the earlier
/// run, with their original order kept.
fn dominant_runs(runs: Vec<Run>) -> Vec<Run> {
    if runs.len() <= 2 {
        return runs;
    }
    let mut ranked: Vec<usize> = (0..runs.len()).collect();
    ranked.sort_by(|&a, &b| runs[b].steps.cmp(&runs[a].steps).then(a.cmp(&b)));
    let mut kept: Vec<usize> = ranked.into_iter().take(2).collect();
    kept.sort_unstable();
    kept.into_iter().map(|index| runs[index]).collect()
}

fn render(runs: &[Run], eighths: &[char; 8]) -> Cell {
    match runs {
        [] => EMPTY,
        [only] => whole_cell(only.tag),
        [earlier, later] => partial_cell(*earlier, *later, eighths),
        _ => EMPTY,
    }
}

fn whole_cell(tag: Option<usize>) -> Cell {
    match tag {
        Some(tag) => solid(tag),
        None => EMPTY,
    }
}

fn solid(tag: usize) -> Cell {
    Cell {
        glyph: FULL_BLOCK,
        fg: Some(tag),
        bg: None,
    }
}

/// Draws the boundary between an earlier and a later run. The glyph's fill is the earlier run, and
/// the background is the later run or nothing. An empty earlier part cannot occur because spans
/// are contiguous from the origin, so it resolves to the nearer of full or empty.
fn partial_cell(earlier: Run, later: Run, eighths: &[char; 8]) -> Cell {
    match (earlier.tag, later.tag) {
        (Some(first), Some(second)) if first == second => solid(first),
        (Some(first), second) => Cell {
            glyph: eighths[earlier.steps],
            fg: Some(first),
            bg: second,
        },
        (None, Some(second)) if later.steps * 2 >= SUB_STEPS => solid(second),
        (None, _) => EMPTY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);

    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 11
        }

        fn unit(&mut self) -> f64 {
            self.next_u64() as f64 / (1u64 << 53) as f64
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() % bound as u64) as usize
        }
    }

    fn seg(value: f64, tag: usize) -> Segment {
        Segment { value, tag }
    }

    fn full(tag: usize) -> Cell {
        Cell {
            glyph: FULL_BLOCK,
            fg: Some(tag),
            bg: None,
        }
    }

    fn partial(glyph: char, fg: usize, bg: Option<usize>) -> Cell {
        Cell {
            glyph,
            fg: Some(fg),
            bg,
        }
    }

    fn mirror(cells: &[Cell]) -> Vec<Cell> {
        cells
            .iter()
            .map(|cell| {
                let glyph = LEFT_EIGHTHS
                    .iter()
                    .position(|&g| g == cell.glyph)
                    .map_or(cell.glyph, |index| LOWER_EIGHTHS[index]);
                Cell { glyph, ..*cell }
            })
            .collect()
    }

    fn horizontal_cases() -> Vec<(Vec<Segment>, f64, usize)> {
        vec![
            (vec![seg(50.0, 0), seg(30.0, 1)], 100.0, 10),
            (vec![seg(37.5, 0), seg(37.5, 1)], 100.0, 4),
            (vec![seg(150.0, 0), seg(50.0, 1)], 100.0, 1),
            (vec![seg(12.5, 0), seg(75.0, 1), seg(12.5, 2)], 100.0, 1),
            (vec![seg(10.0, 0), seg(15.0, 0)], 100.0, 4),
            (vec![seg(60.0, 0), seg(40.0, 1)], 100.0, 1),
            (vec![seg(-50.0, 0), seg(50.0, 1)], 100.0, 4),
            (vec![seg(50.0, 0), seg(f64::NAN, 1)], 100.0, 4),
            (vec![], 100.0, 3),
            (vec![seg(100.0, 2)], 100.0, 1),
            (vec![seg(50.0, 0)], 100.0, 0),
            (vec![seg(50.0, 0)], 100.0, 1),
        ]
    }

    #[test]
    fn horizontal_full_and_empty_cells() {
        let cells = resolve_horizontal(&[seg(50.0, 0), seg(30.0, 1)], 100.0, 10);
        let expected = [
            full(0),
            full(0),
            full(0),
            full(0),
            full(0),
            full(1),
            full(1),
            full(1),
            EMPTY,
            EMPTY,
        ];
        assert_eq!(cells, expected);
    }

    #[test]
    fn horizontal_boundary_inside_a_cell() {
        let cells = resolve_horizontal(&[seg(37.5, 0), seg(37.5, 1)], 100.0, 4);
        let expected = [
            full(0),
            partial(LEFT_EIGHTHS[4], 0, Some(1)),
            full(1),
            EMPTY,
        ];
        assert_eq!(cells, expected);
    }

    #[test]
    fn horizontal_partial_with_empty_remainder() {
        let cells = resolve_horizontal(&[seg(60.0, 0), seg(40.0, 1)], 100.0, 1);
        assert_eq!(cells, [partial(LEFT_EIGHTHS[5], 0, Some(1))]);
    }

    #[test]
    fn horizontal_partial_against_empty() {
        let cells = resolve_horizontal(&[seg(25.0, 3)], 100.0, 1);
        assert_eq!(cells, [partial(LEFT_EIGHTHS[2], 3, None)]);
    }

    #[test]
    fn cumulative_rounding_fills_exactly_ten_cells_for_thirds() {
        let third = 100.0 / 3.0;
        let segments = [seg(third, 0), seg(third, 1), seg(third, 2)];
        let cells = resolve_horizontal(&segments, 100.0, 10);
        assert_eq!(cells.len(), 10);
        assert!(cells.iter().all(|cell| cell.fg.is_some()));
        assert_eq!(spans(&segments, 100.0, 10).last().map(|s| s.end), Some(80));
    }

    #[test]
    fn overflow_is_scaled_proportionally() {
        let cells = resolve_horizontal(&[seg(150.0, 0), seg(50.0, 1)], 100.0, 1);
        assert_eq!(cells, [partial(LEFT_EIGHTHS[6], 0, Some(1))]);
    }

    #[test]
    fn overflow_fills_the_whole_bar() {
        let cells = resolve_horizontal(&[seg(80.0, 0), seg(80.0, 1)], 100.0, 4);
        assert_eq!(cells, [full(0), full(0), full(1), full(1)]);
    }

    #[test]
    fn huge_values_do_not_overflow_the_sum() {
        let cells = resolve_horizontal(&[seg(f64::MAX, 0), seg(f64::MAX, 1)], 100.0, 2);
        assert_eq!(cells, [full(0), full(1)]);
    }

    #[test]
    fn empty_input_is_all_empty_cells() {
        assert_eq!(resolve_horizontal(&[], 100.0, 3), vec![EMPTY; 3]);
        assert_eq!(resolve_vertical(&[], 100.0, 3), vec![EMPTY; 3]);
    }

    #[test]
    fn invalid_values_contribute_nothing() {
        let segments = [
            seg(f64::NAN, 0),
            seg(-5.0, 1),
            seg(f64::INFINITY, 2),
            seg(0.0, 3),
            seg(50.0, 4),
        ];
        let cells = resolve_horizontal(&segments, 100.0, 10);
        let mut expected = vec![full(4); 5];
        expected.extend(vec![EMPTY; 5]);
        assert_eq!(cells, expected);
    }

    #[test]
    fn unusable_totals_are_all_empty() {
        let segments = [seg(10.0, 0)];
        for total in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(resolve_horizontal(&segments, total, 3), vec![EMPTY; 3]);
            assert_eq!(resolve_vertical(&segments, total, 3), vec![EMPTY; 3]);
        }
    }

    #[test]
    fn same_tag_neighbours_merge_into_a_full_cell() {
        let cells = resolve_horizontal(&[seg(10.0, 0), seg(15.0, 0)], 100.0, 4);
        assert_eq!(cells, [full(0), EMPTY, EMPTY, EMPTY]);
    }

    #[test]
    fn three_segments_in_one_cell_keep_the_two_largest_in_order() {
        let cells = resolve_horizontal(&[seg(12.5, 0), seg(75.0, 1), seg(12.5, 2)], 100.0, 1);
        assert_eq!(cells, [partial(LEFT_EIGHTHS[1], 0, Some(1))]);
    }

    #[test]
    fn zero_and_one_sized_bars() {
        assert!(resolve_horizontal(&[seg(50.0, 0)], 100.0, 0).is_empty());
        assert!(resolve_vertical(&[seg(50.0, 0)], 100.0, 0).is_empty());
        assert_eq!(resolve_horizontal(&[seg(100.0, 2)], 100.0, 1), [full(2)]);
    }

    #[test]
    fn vertical_mirrors_horizontal_for_every_case() {
        for (segments, total, size) in horizontal_cases() {
            let horizontal = resolve_horizontal(&segments, total, size);
            let vertical = resolve_vertical(&segments, total, size);
            assert_eq!(
                vertical,
                mirror(&horizontal),
                "case {segments:?} {total} {size}"
            );
        }
    }

    #[test]
    fn vertical_boundary_uses_lower_eighths() {
        let cells = resolve_vertical(&[seg(37.5, 0), seg(37.5, 1)], 100.0, 4);
        let expected = [
            full(0),
            partial(LOWER_EIGHTHS[4], 0, Some(1)),
            full(1),
            EMPTY,
        ];
        assert_eq!(cells, expected);
    }

    #[test]
    fn vertical_single_row_partial() {
        let cells = resolve_vertical(&[seg(12.5, 0), seg(75.0, 1), seg(12.5, 2)], 100.0, 1);
        assert_eq!(cells, [partial(LOWER_EIGHTHS[1], 0, Some(1))]);
    }

    #[test]
    fn random_segment_sets_respect_invariants() {
        let mut rng = Lcg(0x5eed_1234);
        for case in 0..500 {
            let cells = rng.below(13);
            let total = [1.0, 100.0, 1.0 + 999.0 * rng.unit()][rng.below(3)];
            let count = rng.below(7);
            let segments: Vec<Segment> = (0..count)
                .map(|tag| seg(random_value(&mut rng, total), tag))
                .collect();

            let horizontal = resolve_horizontal(&segments, total, cells);
            let vertical = resolve_vertical(&segments, total, cells);
            assert_eq!(horizontal.len(), cells, "case {case}");
            assert_eq!(vertical, mirror(&horizontal), "case {case}");

            let valid_sum: f64 = segments
                .iter()
                .filter(|s| s.value.is_finite() && s.value > 0.0)
                .map(|s| s.value)
                .sum();
            let fraction = valid_sum / total;
            let filled = spans(&segments, total, cells)
                .last()
                .map_or(0, |span| span.end) as f64;
            let expected_filled = fraction.min(1.0) * cells as f64 * SUB_STEPS as f64;
            assert!(
                (filled - expected_filled).abs() <= 1.0,
                "case {case}: filled {filled} expected {expected_filled}"
            );

            let non_empty = horizontal.iter().filter(|cell| cell.fg.is_some()).count();
            assert!(
                non_empty as f64 <= (fraction * cells as f64).ceil() + 1.0,
                "case {case}: {non_empty} non-empty cells"
            );
        }
    }

    fn random_value(rng: &mut Lcg, total: f64) -> f64 {
        match rng.below(20) {
            0 => -total * rng.unit(),
            1 => f64::NAN,
            2 => 0.0,
            3 => f64::INFINITY,
            _ => total * 0.6 * rng.unit(),
        }
    }
}
