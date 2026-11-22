//! 토끼 아이콘 — 단순한 그림자 실루엣 (tiny-skia, 알파 마스크 출력).
//!
//! 얼굴 없이 꽉 찬 실루엣 하나. 색은 플랫폼이 입힙니다(작업 표시줄 테마, macOS 템플릿 이미지).
//! 귀 두 개가 게이지: 왼쪽 = 세션(5시간), 오른쪽 = 주간. 많이 쓸수록 바깥으로 처짐.
//! 땀방울·Zzz·앞발은 주변에 1px 틈을 파서 실루엣과 분리합니다.

use crate::animator::{Burrow, Pose};
use tiny_skia::{BlendMode, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// 사용률 → 귀가 바깥으로 기운 각도(도). 0%는 살짝 V, 100%는 완전히 처짐.
pub fn ear_angle(ratio: f32) -> f32 {
    8.0 + 102.0 * ratio.clamp(0.0, 1.0).powf(1.35)
}

#[derive(Clone, Copy)]
struct Ell {
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    ang: f32,
}

impl Ell {
    fn new(cx: f32, cy: f32, rx: f32, ry: f32) -> Self {
        Self { cx, cy, rx, ry, ang: 0.0 }
    }
    fn pts(&self, grow: f32) -> Vec<(f32, f32)> {
        let (rx, ry) = ((self.rx + grow).max(0.1), (self.ry + grow).max(0.1));
        let (s, c) = self.ang.to_radians().sin_cos();
        (0..56)
            .map(|i| {
                let t = std::f32::consts::TAU * i as f32 / 56.0;
                let (x, y) = (rx * t.cos(), ry * t.sin());
                (self.cx + x * c - y * s, self.cy + x * s + y * c)
            })
            .collect()
    }
}

pub(crate) struct Canvas {
    pub pm: Pixmap,
    scale: f32,
    pub gap: f32,
}

impl Canvas {
    pub fn new(size: u32, units: f32) -> Self {
        let size = size.max(1);
        Self {
            pm: Pixmap::new(size, size).expect("pixmap"),
            scale: size as f32 / units,
            gap: (105.0 / size as f32).max(2.6) * units / 100.0,
        }
    }

    fn paint(add: bool) -> Paint<'static> {
        let mut p = Paint::default();
        p.anti_alias = true;
        p.set_color_rgba8(255, 255, 255, 255);
        if !add {
            p.blend_mode = BlendMode::Clear;
        }
        p
    }

    fn poly(&mut self, pts: &[(f32, f32)], add: bool) {
        let mut pb = PathBuilder::new();
        let Some(&(x0, y0)) = pts.first() else { return };
        pb.move_to(x0, y0);
        for &(x, y) in &pts[1..] {
            pb.line_to(x, y);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.pm.fill_path(&path, &Self::paint(add), FillRule::Winding, Transform::from_scale(self.scale, self.scale), None);
        }
    }

    pub fn fill_path(&mut self, path: &tiny_skia::Path, add: bool, ts: Transform) {
        self.pm.fill_path(path, &Self::paint(add), FillRule::Winding, ts, None);
    }

    fn add(&mut self, pts: &[(f32, f32)]) {
        self.poly(pts, true)
    }
    fn cut(&mut self, pts: &[(f32, f32)]) {
        self.poly(pts, false)
    }

    fn line(&mut self, pts: &[(f32, f32)], width: f32, add: bool) {
        let mut pb = PathBuilder::new();
        let Some(&(x0, y0)) = pts.first() else { return };
        pb.move_to(x0, y0);
        for &(x, y) in &pts[1..] {
            pb.line_to(x, y);
        }
        let Some(path) = pb.finish() else { return };
        let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
        self.pm.stroke_path(&path, &Self::paint(add), &stroke, Transform::from_scale(self.scale, self.scale), None);
    }

    /// 알파 채널만 꺼냄 (size*size 바이트)
    pub fn alpha(&self) -> Vec<u8> {
        self.pm.data().chunks_exact(4).map(|p| p[3]).collect()
    }
}

/// 포즈 → 알파 마스크 (size x size, 행 우선)
pub fn render(pose: &Pose, size: u32) -> Vec<u8> {
    let mut c = Canvas::new(size, 100.0);
    match pose.burrow {
        Burrow::Sleep => burrow_sleep(&mut c, pose),
        Burrow::Peek => burrow_peek(&mut c),
        Burrow::None => rabbit(&mut c, pose),
    }
    c.alpha()
}

fn ears(pose: &Pose, base_l: (f32, f32), base_r: (f32, f32), length: f32, width: f32, max_angle: Option<f32>) -> [Ell; 2] {
    let make = |side: f32, base: (f32, f32), ratio: f32, extra: i8| {
        let mut tilt = ear_angle(ratio);
        if let Some(m) = max_angle {
            tilt = tilt.min(m);
        }
        let ang = side * (tilt + extra as f32 * 2.5);
        let (s, c) = ang.to_radians().sin_cos();
        let (dx, dy) = (s, -c);
        Ell { cx: base.0 + dx * length * 0.5, cy: base.1 + dy * length * 0.5, rx: width, ry: length * 0.56, ang }
    };
    [make(-1.0, base_l, pose.session_ratio(), pose.ear_l), make(1.0, base_r, pose.weekly_ratio(), pose.ear_r)]
}

fn rabbit(c: &mut Canvas, pose: &Pose) {
    let sq = pose.squash as f32 * 0.025;
    let lift = pose.lift as f32;
    let t = |x: f32, y: f32| (50.0 + (x - 50.0) * (1.0 + sq * 0.6), 96.0 - lift - (96.0 - y) * (1.0 - sq));
    let e = |el: Ell| {
        let (cx, cy) = t(el.cx, el.cy);
        Ell { cx, cy, rx: el.rx * (1.0 + sq * 0.6), ry: el.ry * (1.0 - sq), ang: el.ang }
    };
    let head = e(Ell::new(50.0, 60.0, 26.0, 22.0));
    let body = e(Ell::new(50.0, 82.0, 23.0, 14.0));
    let feet = [e(Ell::new(37.0, 92.5, 9.0, 4.2)), e(Ell::new(63.0, 92.5, 9.0, 4.2))];
    for ear in ears(pose, t(41.0, 45.0), t(59.0, 45.0), 38.0, 8.4, None) {
        c.add(&ear.pts(0.0));
    }
    for shape in [body, feet[0], feet[1], head] {
        c.add(&shape.pts(0.0));
    }

    if let Some(p) = pose.paw {
        let ph = p as f32 / 6.0;
        let px = 36.0 + 5.0 * (ph * std::f32::consts::TAU).sin();
        let py = 72.0 - 6.0 * (ph * std::f32::consts::PI).sin().abs();
        let paw = e(Ell::new(px, py, 6.0, 5.0));
        c.cut(&paw.pts(c.gap));
        c.add(&paw.pts(0.0));
    }
    if let Some(s) = pose.sweat {
        sweat(c, t(78.0, 40.0 + s as f32 / 8.0 * 12.0));
    }
    if let Some(z) = pose.zzz {
        zzz(c, z as f32 / 8.0, (66.0, 38.0));
    }
    if lift >= 3.0 {
        let w = 18.0 * (1.0 - lift / 24.0).max(0.45);
        let g = c.gap.max(2.4);
        c.line(&[(50.0 - w, 98.5), (50.0 + w, 98.5)], g, true);
    }
}

fn sweat(c: &mut Canvas, (x, y): (f32, f32)) {
    let g = c.gap;
    c.cut(&Ell::new(x, y + 2.5, 4.2 + g, 4.6 + g).pts(0.0));
    c.cut(&[(x - 3.6 - g, y + 1.6), (x, y - 7.0 - g * 1.4), (x + 3.6 + g, y + 1.6)]);
    c.add(&Ell::new(x, y + 2.5, 4.2, 4.6).pts(0.0));
    c.add(&[(x - 3.6, y + 1.6), (x, y - 7.0), (x + 3.6, y + 1.6)]);
}

fn zzz(c: &mut Canvas, phase: f32, (ox, oy): (f32, f32)) {
    let w = c.gap.max(2.6);
    for (i, size) in [9.0f32, 12.0].into_iter().enumerate() {
        let p = (phase + i as f32 * 0.5) % 1.0;
        let x = ox + i as f32 * 12.0 + 4.0 * p;
        let y = oy - i as f32 * 14.0 - 12.0 * p;
        let z = [(x, y), (x + size, y), (x, y + size), (x + size, y + size)];
        c.line(&z, w + c.gap * 2.0, false);
        c.line(&z, w, true);
    }
}

fn burrow_sleep(c: &mut Canvas, pose: &Pose) {
    // 둥근 언덕(굴) 뒤로 처진 귀 끝만 보이고 Zzz
    for ear in ears(pose, (43.0, 76.0), (57.0, 76.0), 30.0, 7.6, Some(62.0)) {
        c.add(&ear.pts(0.0));
    }
    let mound = Ell::new(50.0, 104.0, 46.0, 30.0);
    c.cut(&mound.pts(c.gap));
    c.add(&mound.pts(0.0));
    c.cut(&Ell::new(50.0, 101.0, 13.0, 12.0).pts(0.0)); // 아치형 굴 입구
    zzz(c, pose.zzz.unwrap_or(0) as f32 / 8.0, (68.0, 40.0));
}

fn burrow_peek(c: &mut Canvas) {
    // 리셋 직후: 굴에서 고개를 쏙
    for ear in ears(&Pose::default(), (42.0, 59.0), (58.0, 59.0), 32.0, 7.4, None) {
        c.add(&ear.pts(0.0));
    }
    c.add(&Ell::new(50.0, 72.0, 22.0, 18.0).pts(0.0));
    let lip = Ell::new(50.0, 114.0, 49.0, 24.0);
    c.cut(&lip.pts(c.gap));
    c.add(&lip.pts(0.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ears_droop_with_usage() {
        assert!(ear_angle(0.0) < ear_angle(0.5));
        assert!(ear_angle(0.5) < ear_angle(1.0));
        assert_eq!(ear_angle(1.0), ear_angle(1.7));
    }

    #[test]
    fn renders_all_sizes_and_poses() {
        let poses = [
            Pose::gauges(0.1, 0.05),
            Pose { lift: 8, squash: -2, ear_l: 4, ear_r: 4, ..Pose::gauges(0.5, 0.3) },
            Pose { sweat: Some(3), ..Pose::gauges(0.9, 0.5) },
            Pose { paw: Some(2), ..Pose::gauges(0.2, 0.2) },
            Pose { burrow: Burrow::Sleep, zzz: Some(2), ..Pose::gauges(1.0, 0.6) },
            Pose { burrow: Burrow::Peek, ..Default::default() },
        ];
        for size in [16u32, 20, 24, 32, 64] {
            for p in &poses {
                let a = render(p, size);
                assert_eq!(a.len(), (size * size) as usize);
                assert!(a.iter().any(|&v| v > 200), "empty render at {size}");
            }
        }
    }
}

/// 앱 아이콘 (검은 둥근 사각형 + 흰 토끼 실루엣). tiny-skia 픽스맵 데이터(premultiplied RGBA) 그대로.
/// exe 아이콘(빌드 때), 알림 아이콘에 같이 씀.
pub fn app_icon(n: u32) -> Option<tiny_skia::Pixmap> {
    use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};
    let mut pm = Pixmap::new(n, n)?;
    let s = n as f32;
    let r = s * 0.23;
    let mut pb = PathBuilder::new();
    pb.move_to(r, 0.0);
    pb.line_to(s - r, 0.0);
    pb.quad_to(s, 0.0, s, r);
    pb.line_to(s, s - r);
    pb.quad_to(s, s, s - r, s);
    pb.line_to(r, s);
    pb.quad_to(0.0, s, 0.0, s - r);
    pb.line_to(0.0, r);
    pb.quad_to(0.0, 0.0, r, 0.0);
    pb.close();
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.set_color_rgba8(0x14, 0x14, 0x14, 255);
    pm.fill_path(&pb.finish()?, &paint, FillRule::Winding, Transform::identity(), None);
    // 흰 토끼: 작은 크기에서도 보이게 비율을 조금 키움
    let size = ((s * if n <= 24 { 0.86 } else { 0.74 }).round() as u32).max(8);
    let mask = render(&Pose::default(), size);
    let off = (n - size) / 2;
    let data = pm.data_mut();
    for y in 0..size {
        for x in 0..size {
            let a = mask[(y * size + x) as usize] as u32;
            if a == 0 {
                continue;
            }
            let i = (((y + off) * n + (x + off)) * 4) as usize;
            for c in 0..4 {
                data[i + c] = ((255 * a + data[i + c] as u32 * (255 - a)) / 255) as u8;
            }
        }
    }
    Some(pm)
}
