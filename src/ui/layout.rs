//! Splits the terminal into panels, dropping the least important ones first as space runs out.

use ratatui::layout::Rect;

pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 14;
const WIDE_WIDTH: u16 = 100;
const MIN_TABLE_WIDTH: u16 = 56;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Regions {
    pub title: Rect,
    pub story: Rect,
    pub bars: Rect,
    pub table: Rect,
    pub timeline: Option<Rect>,
    pub detail: Option<Rect>,
    pub hardware: Option<Rect>,
    pub footer: Rect,
}

/// How many text lines the headline and findings need, supplied by the caller so layout stays free of content.
#[derive(Clone, Copy, Debug)]
pub struct StoryNeeds {
    pub headline_lines: u16,
    pub note_lines: u16,
}

pub fn regions(area: Rect, needs: StoryNeeds) -> Option<Regions> {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }
    let notes = if area.height >= 26 {
        needs.note_lines.min(3)
    } else {
        0
    };
    let story_height = needs.headline_lines.clamp(1, 2) + notes;
    let bars_height = 2;
    let mut hardware_height = match area.height {
        0..=21 => 0,
        22..=29 => 1,
        _ => 2,
    };
    let mut detail_height = if area.height >= 34 { 8 } else { 0 };
    let fixed = 1 + story_height + bars_height + 1;
    let min_main = 6;
    while fixed + hardware_height + detail_height + min_main > area.height {
        if detail_height > 0 {
            detail_height = 0;
        } else if hardware_height > 0 {
            hardware_height = 0;
        } else {
            break;
        }
    }
    let mut y = area.y;
    let mut take = |height: u16| {
        let rect = Rect::new(area.x, y, area.width, height);
        y += height;
        rect
    };
    let title = take(1);
    let story = take(story_height);
    let bars = take(bars_height);
    let main_height = area.height - fixed - hardware_height - detail_height;
    let main = take(main_height);
    let detail = (detail_height > 0).then(|| take(detail_height));
    let hardware = (hardware_height > 0).then(|| take(hardware_height));
    let footer = take(1);
    let (table, timeline) = split_main(main);
    Some(Regions {
        title,
        story,
        bars,
        table,
        timeline,
        detail,
        hardware,
        footer,
    })
}

fn split_main(main: Rect) -> (Rect, Option<Rect>) {
    if main.width < WIDE_WIDTH {
        return (main, None);
    }
    let table_width = (main.width * 55 / 100).max(MIN_TABLE_WIDTH);
    let table = Rect::new(main.x, main.y, table_width, main.height);
    let timeline = Rect::new(
        main.x + table_width,
        main.y,
        main.width - table_width,
        main.height,
    );
    (table, Some(timeline))
}

/// A rectangle of the given size centred inside `area`, clamped to fit.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEEDS: StoryNeeds = StoryNeeds {
        headline_lines: 2,
        note_lines: 3,
    };

    fn stacked_heights(r: &Regions) -> u16 {
        r.title.height
            + r.story.height
            + r.bars.height
            + r.table.height
            + r.detail.map_or(0, |d| d.height)
            + r.hardware.map_or(0, |h| h.height)
            + r.footer.height
    }

    #[test]
    fn too_small_terminals_get_no_layout() {
        assert!(regions(Rect::new(0, 0, 59, 40), NEEDS).is_none());
        assert!(regions(Rect::new(0, 0, 120, 13), NEEDS).is_none());
    }

    #[test]
    fn every_size_fills_the_screen_exactly() {
        for width in [60, 80, 99, 100, 140, 220] {
            for height in 14..60 {
                let area = Rect::new(0, 0, width, height);
                let r = regions(area, NEEDS).expect("layout");
                assert_eq!(stacked_heights(&r), height, "{width}x{height}");
                assert!(
                    r.table.height >= 4,
                    "{width}x{height}: table {}",
                    r.table.height
                );
                assert!(r.footer.y + r.footer.height == height);
            }
        }
    }

    #[test]
    fn wide_terminals_put_the_timeline_beside_the_table() {
        let r = regions(Rect::new(0, 0, 140, 40), NEEDS).unwrap();
        let timeline = r.timeline.unwrap();
        assert_eq!(r.table.x + r.table.width, timeline.x);
        assert_eq!(r.table.width + timeline.width, 140);
        assert!(r.detail.is_some() && r.hardware.is_some());
    }

    #[test]
    fn narrow_terminals_drop_the_timeline() {
        let r = regions(Rect::new(0, 0, 80, 40), NEEDS).unwrap();
        assert!(r.timeline.is_none());
        assert_eq!(r.table.width, 80);
    }

    #[test]
    fn short_terminals_drop_extras_in_order() {
        let r = regions(Rect::new(0, 0, 120, 20), NEEDS).unwrap();
        assert!(r.detail.is_none() && r.hardware.is_none());
        let r = regions(Rect::new(0, 0, 120, 24), NEEDS).unwrap();
        assert!(r.detail.is_none() && r.hardware.is_some());
    }

    #[test]
    fn centered_clamps() {
        let area = Rect::new(0, 0, 50, 10);
        assert_eq!(centered(area, 20, 4), Rect::new(15, 3, 20, 4));
        assert_eq!(centered(area, 90, 40), area);
    }
}
