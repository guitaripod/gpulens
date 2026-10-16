//! The strip along the bottom: power, heat, clocks, bus traffic and whether the card is being held back.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::widgets;
use crate::app::App;
use crate::format;
use crate::types::{DeviceInfo, DeviceSample, Throttle};

pub fn render(buf: &mut Buffer, area: Rect, app: &App) {
    let device = &app.model.devices[app.device];
    let Some(raw) = device.sample.as_ref() else {
        return;
    };
    let mut calm = raw.clone();
    calm.power_w = device.power_avg.map(|v| v as f32);
    calm.pcie_tx_kbps = device.pcie_tx_avg.map(|v| v.round() as u32);
    calm.pcie_rx_kbps = device.pcie_rx_avg.map(|v| v.round() as u32);
    calm.throttle = Throttle {
        unknown: raw.throttle.unknown,
        ..device.calm_throttle
    };
    let sample = &calm;
    let mut first = Row::new(buf, area, 0, app);
    first.power(sample, &device.info);
    first.temperature(sample, &device.info);
    first.clocks(sample, &device.info);
    first.fan(sample);
    if area.height >= 2 {
        let mut second = Row::new(buf, area, 1, app);
        second.bus(sample, &device.info);
        second.video(sample);
        second.status(sample);
    }
}

/// Lays chips out left to right and stops when the line is full.
struct Row<'a> {
    buf: &'a mut Buffer,
    app: &'a App,
    x: u16,
    y: u16,
    right: u16,
}

impl<'a> Row<'a> {
    fn new(buf: &'a mut Buffer, area: Rect, line: u16, app: &'a App) -> Self {
        Self {
            buf,
            app,
            x: area.x + 1,
            y: area.y + line,
            right: area.x + area.width,
        }
    }

    fn room(&self) -> u16 {
        self.right.saturating_sub(self.x)
    }

    fn put(&mut self, text: &str, style: Style) {
        let used = widgets::text(self.buf, self.x, self.y, text, style, self.room());
        self.x += used;
    }

    fn chip(&mut self, label: &str, value: &str, style: Style) {
        let needed = (label.chars().count() + value.chars().count() + 4) as u16;
        if needed > self.room() {
            return;
        }
        let dim = self.app.theme.dim();
        self.put(label, dim);
        self.put(" ", dim);
        self.put(value, style);
        self.put("   ", dim);
    }

    fn power(&mut self, sample: &DeviceSample, info: &DeviceInfo) {
        let Some(draw) = sample.power_w else { return };
        let limit = sample
            .power_limit_w
            .or(info.power_limit_w)
            .unwrap_or(draw.max(1.0));
        let fraction = f64::from(draw / limit.max(1.0));
        let text = format!("{draw:.0}/{limit:.0} W");
        if self.room() < 22 {
            return;
        }
        let theme = &self.app.theme;
        let heat = theme.heat(fraction);
        self.put("Power ", theme.dim());
        widgets::gauge(
            self.buf,
            Rect::new(self.x, self.y, 8, 1),
            fraction,
            heat,
            theme,
        );
        self.x += 9;
        self.put(&text, heat);
        self.put("   ", theme.dim());
    }

    fn temperature(&mut self, sample: &DeviceSample, info: &DeviceInfo) {
        let Some(temp) = sample.temp_c else { return };
        let limit = f64::from(info.slowdown_temp_c.unwrap_or(90));
        let style = self.app.theme.heat(f64::from(temp) / limit);
        self.chip("Temp", &format!("{temp}°C"), style);
    }

    fn clocks(&mut self, sample: &DeviceSample, info: &DeviceInfo) {
        let Some(now) = sample.sm_clock_mhz else {
            return;
        };
        let value = match info.max_sm_clock_mhz {
            Some(max) => format!("{} / {}", format::clock_mhz(now), format::clock_mhz(max)),
            None => format::clock_mhz(now),
        };
        let style = self.app.theme.text();
        self.chip("Core clock", &value, style);
        if let Some(memory) = sample.mem_clock_mhz {
            self.chip("Memory clock", &format::clock_mhz(memory), style);
        }
    }

    fn fan(&mut self, sample: &DeviceSample) {
        if let Some(fan) = sample.fan_pct {
            let style = self.app.theme.text();
            self.chip("Fan", &format!("{fan}%"), style);
        }
    }

    fn bus(&mut self, sample: &DeviceSample, info: &DeviceInfo) {
        let (Some(tx), Some(rx)) = (sample.pcie_tx_kbps, sample.pcie_rx_kbps) else {
            return;
        };
        let glyphs = &self.app.theme.glyphs;
        let link = match (info.pcie_max_gen, info.pcie_max_width) {
            (Some(gen_), Some(width)) => format!("  (Gen{gen_} x{width})"),
            _ => String::new(),
        };
        let value = format!(
            "{}{} {}{}{link}",
            glyphs.arrow_up,
            format::rate_kbps(tx),
            glyphs.arrow_down,
            format::rate_kbps(rx)
        );
        let style = self.app.theme.text();
        self.chip("PCIe", &value, style);
    }

    fn video(&mut self, sample: &DeviceSample) {
        if let (Some(enc), Some(dec)) = (sample.enc_util, sample.dec_util) {
            let style = self.app.theme.text();
            self.chip("Video encode/decode", &format!("{enc}% / {dec}%"), style);
        }
    }

    fn status(&mut self, sample: &DeviceSample) {
        if sample.throttle.unknown {
            return;
        }
        let theme = &self.app.theme;
        let (text, style) = if sample.throttle.thermal {
            ("too hot, slowing down", theme.bad())
        } else if sample.throttle.hardware {
            ("hardware slowdown", theme.bad())
        } else if sample.throttle.power {
            ("at its power limit", theme.warn())
        } else if sample.throttle.idle {
            ("resting (idle clocks)", theme.dim())
        } else {
            ("running at full speed", theme.good())
        };
        self.chip("Status", text, style);
    }
}
