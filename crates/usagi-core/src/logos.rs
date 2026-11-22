//! 공급자 로고 (위젯/메뉴에서 어느 서비스인지 보여 줌).
//!
//! 경로 데이터: simple-icons 13.21.0 (CC0 1.0, https://simpleicons.org).
//! 로고 자체는 각 회사의 상표이며, 어떤 서비스의 사용량인지 표시하는 용도로만 씁니다.

use crate::engine::ProviderId;
use crate::rabbit::Canvas;
use tiny_skia::{Path, PathBuilder, Transform};

const CLAUDE: &str = "m4.7144 15.9555 4.7174-2.6471.079-.2307-.079-.1275h-.2307l-.7893-.0486-2.6956-.0729-2.3375-.0971-2.2646-.1214-.5707-.1215-.5343-.7042.0546-.3522.4797-.3218.686.0608 1.5179.1032 2.2767.1578 1.6514.0972 2.4468.255h.3886l.0546-.1579-.1336-.0971-.1032-.0972L6.973 9.8356l-2.55-1.6879-1.3356-.9714-.7225-.4918-.3643-.4614-.1578-1.0078.6557-.7225.8803.0607.2246.0607.8925.686 1.9064 1.4754 2.4893 1.8336.3643.3035.1457-.1032.0182-.0728-.164-.2733-1.3539-2.4467-1.445-2.4893-.6435-1.032-.17-.6194c-.0607-.255-.1032-.4674-.1032-.7285L6.287.1335 6.6997 0l.9957.1336.419.3642.6192 1.4147 1.0018 2.2282 1.5543 3.0296.4553.8985.2429.8318.091.255h.1579v-.1457l.1275-1.706.2368-2.0947.2307-2.6957.0789-.7589.3764-.9107.7468-.4918.5828.2793.4797.686-.0668.4433-.2853 1.8517-.5586 2.9021-.3643 1.9429h.2125l.2429-.2429.9835-1.3053 1.6514-2.0643.7286-.8196.85-.9046.5464-.4311h1.0321l.759 1.1293-.34 1.1657-1.0625 1.3478-.8804 1.1414-1.2628 1.7-.7893 1.36.0729.1093.1882-.0183 2.8535-.607 1.5421-.2794 1.8396-.3157.8318.3886.091.3946-.3278.8075-1.967.4857-2.3072.4614-3.4364.8136-.0425.0304.0486.0607 1.5482.1457.6618.0364h1.621l3.0175.2247.7892.522.4736.6376-.079.4857-1.2142.6193-1.6393-.3886-3.825-.9107-1.3113-.3279h-.1822v.1093l1.0929 1.0686 2.0035 1.8092 2.5075 2.3314.1275.5768-.3218.4554-.34-.0486-2.2039-1.6575-.85-.7468-1.9246-1.621h-.1275v.17l.4432.6496 2.3436 3.5214.1214 1.0807-.17.3521-.6071.2125-.6679-.1214-1.3721-1.9246L14.38 17.959l-1.1414-1.9428-.1397.079-.674 7.2552-.3156.3703-.7286.2793-.6071-.4614-.3218-.7468.3218-1.4753.3886-1.9246.3157-1.53.2853-1.9004.17-.6314-.0121-.0425-.1397.0182-1.4328 1.9672-2.1796 2.9446-1.7243 1.8456-.4128.164-.7164-.3704.0667-.6618.4008-.5889 2.386-3.0357 1.4389-1.882.929-1.0868-.0062-.1579h-.0546l-6.3385 4.1164-1.1293.1457-.4857-.4554.0608-.7467.2307-.2429 1.9064-1.3114Z";

const OPENAI: &str = "M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997Z";

/// 로고 고유색 (None이면 테마 색으로 칠함: OpenAI는 흑백 로고)
pub fn brand_color(id: ProviderId) -> Option<(u8, u8, u8)> {
    match id {
        ProviderId::Claude => Some((0xD9, 0x77, 0x57)),
        ProviderId::Codex => None,
    }
}

/// 로고 → 알파 마스크 (size x size). 24x24 viewBox 기준.
pub fn render(id: ProviderId, size: u32) -> Vec<u8> {
    let d = match id {
        ProviderId::Claude => CLAUDE,
        ProviderId::Codex => OPENAI,
    };
    let mut c = Canvas::new(size, 24.0);
    if let Some(path) = parse_svg_path(d) {
        let s = size as f32 / 24.0;
        c.fill_path(&path, true, Transform::from_scale(s, s));
    }
    c.alpha()
}

// ------------------------------------------------------------------ SVG path

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl Lexer<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b',' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }
    fn command(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        if c.is_ascii_alphabetic() && c != b'e' && c != b'E' {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }
    fn has_number(&mut self) -> bool {
        self.skip();
        matches!(self.s.get(self.i), Some(c) if c.is_ascii_digit() || *c == b'-' || *c == b'+' || *c == b'.')
    }
    fn number(&mut self) -> Option<f32> {
        self.skip();
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
            self.i += 1;
        }
        let mut dot = false;
        while let Some(&c) = self.s.get(self.i) {
            if c.is_ascii_digit() {
                self.i += 1;
            } else if c == b'.' && !dot {
                dot = true;
                self.i += 1;
            } else {
                break;
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
                self.i += 1;
            }
            while matches!(self.s.get(self.i), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }
    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
}

/// SVG path d 문자열 → tiny-skia Path. M/L/H/V/C/S/Q/T/A/Z (상대/절대) 지원.
pub fn parse_svg_path(d: &str) -> Option<Path> {
    let mut lx = Lexer { s: d.as_bytes(), i: 0 };
    let mut pb = PathBuilder::new();
    let (mut x, mut y, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut last_ctrl: Option<(f32, f32)> = None;
    let mut cmd = lx.command()?;
    loop {
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        let mut ctrl = None;
        match cmd.to_ascii_uppercase() {
            b'M' => {
                x = ox + lx.number()?;
                y = oy + lx.number()?;
                pb.move_to(x, y);
                (sx, sy) = (x, y);
                cmd = if rel { b'l' } else { b'L' }; // 이어지는 좌표는 선
            }
            b'L' => {
                x = ox + lx.number()?;
                y = oy + lx.number()?;
                pb.line_to(x, y);
            }
            b'H' => {
                x = if rel { x } else { 0.0 } + lx.number()?;
                pb.line_to(x, y);
            }
            b'V' => {
                y = if rel { y } else { 0.0 } + lx.number()?;
                pb.line_to(x, y);
            }
            b'C' => {
                let (x1, y1) = (ox + lx.number()?, oy + lx.number()?);
                let (x2, y2) = (ox + lx.number()?, oy + lx.number()?);
                (x, y) = (ox + lx.number()?, oy + lx.number()?);
                pb.cubic_to(x1, y1, x2, y2, x, y);
                ctrl = Some((x2, y2));
            }
            b'S' => {
                let (x1, y1) = last_ctrl.map(|(cx, cy)| (2.0 * x - cx, 2.0 * y - cy)).unwrap_or((x, y));
                let (x2, y2) = (ox + lx.number()?, oy + lx.number()?);
                (x, y) = (ox + lx.number()?, oy + lx.number()?);
                pb.cubic_to(x1, y1, x2, y2, x, y);
                ctrl = Some((x2, y2));
            }
            b'Q' => {
                let (x1, y1) = (ox + lx.number()?, oy + lx.number()?);
                (x, y) = (ox + lx.number()?, oy + lx.number()?);
                pb.quad_to(x1, y1, x, y);
                ctrl = Some((x1, y1));
            }
            b'T' => {
                let (x1, y1) = last_ctrl.map(|(cx, cy)| (2.0 * x - cx, 2.0 * y - cy)).unwrap_or((x, y));
                (x, y) = (ox + lx.number()?, oy + lx.number()?);
                pb.quad_to(x1, y1, x, y);
                ctrl = Some((x1, y1));
            }
            b'A' => {
                let (rx, ry, rot) = (lx.number()?, lx.number()?, lx.number()?);
                let (large, sweep) = (lx.flag()?, lx.flag()?);
                let (x2, y2) = (ox + lx.number()?, oy + lx.number()?);
                arc_to(&mut pb, (x, y), rx, ry, rot, large, sweep, (x2, y2));
                (x, y) = (x2, y2);
            }
            b'Z' => {
                pb.close();
                (x, y) = (sx, sy);
            }
            _ => return None,
        }
        last_ctrl = ctrl;
        if cmd.to_ascii_uppercase() != b'Z' && lx.has_number() {
            continue; // 같은 명령 반복
        }
        match lx.command() {
            Some(c) => cmd = c,
            None => break,
        }
    }
    pb.finish()
}

/// SVG 원호 → 3차 베지어 (SVG 1.1 부록 F.6.5)
#[allow(clippy::too_many_arguments)]
fn arc_to(pb: &mut PathBuilder, (x1, y1): (f32, f32), rx: f32, ry: f32, rot: f32, large: bool, sweep: bool, (x2, y2): (f32, f32)) {
    if (x1 - x2).abs() < 1e-6 && (y1 - y2).abs() < 1e-6 {
        return;
    }
    let (mut rx, mut ry) = (rx.abs() as f64, ry.abs() as f64);
    if rx == 0.0 || ry == 0.0 {
        pb.line_to(x2, y2);
        return;
    }
    let (x1, y1, x2, y2) = (x1 as f64, y1 as f64, x2 as f64, y2 as f64);
    let (sin, cos) = (rot as f64).to_radians().sin_cos();
    let (dx2, dy2) = ((x1 - x2) / 2.0, (y1 - y2) / 2.0);
    let x1p = cos * dx2 + sin * dy2;
    let y1p = -sin * dx2 + cos * dy2;
    let lambda = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = (num / den).max(0.0).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;
    let cx = cos * cxp - sin * cyp + (x1 + x2) / 2.0;
    let cy = sin * cxp + cos * cyp + (y1 + y2) / 2.0;
    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let (ux, uy) = ((x1p - cxp) / rx, (y1p - cyp) / ry);
    let (vx, vy) = ((-x1p - cxp) / rx, (-y1p - cyp) / ry);
    let theta1 = angle(1.0, 0.0, ux, uy);
    let mut dtheta = angle(ux, uy, vx, vy);
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f64::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f64::consts::TAU;
    }
    let segs = (dtheta.abs() / std::f64::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let delta = dtheta / segs as f64;
    let k = 4.0 / 3.0 * (delta / 4.0).tan();
    let map = |ux: f64, uy: f64| {
        ((cx + rx * ux * cos - ry * uy * sin) as f32, (cy + rx * ux * sin + ry * uy * cos) as f32)
    };
    let mut a1 = theta1;
    for _ in 0..segs {
        let a2 = a1 + delta;
        let (s1, c1) = a1.sin_cos();
        let (s2, c2) = a2.sin_cos();
        let p1 = map(c1 - k * s1, s1 + k * c1);
        let p2 = map(c2 + k * s2, s2 - k * c2);
        let p3 = map(c2, s2);
        pb.cubic_to(p1.0, p1.1, p2.0, p2.1, p3.0, p3.1);
        a1 = a2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logos_parse_and_render() {
        for id in [ProviderId::Claude, ProviderId::Codex] {
            let a = render(id, 32);
            let filled = a.iter().filter(|&&v| v > 128).count();
            assert!(filled > 150 && filled < 32 * 32 - 100, "{id:?}: {filled}");
        }
        let p = parse_svg_path(OPENAI).unwrap();
        let b = p.bounds();
        assert!(b.left() > -1.0 && b.right() < 25.0 && b.top() > -1.0 && b.bottom() < 25.0, "{b:?}");
    }
}
