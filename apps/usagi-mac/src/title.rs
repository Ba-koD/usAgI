//! 메뉴 막대 글자 꾸미기: [로고] 5h ▬▬ 53%  7d ▬▬ 64%   [로고] 7d ▬▬ 92%
//!
//! 메뉴 막대 버튼은 AppKit이 서식 있는 텍스트로 그림. 로고와 막대는 글자 사이의 작은 첨부 이미지.
//! 설정 창 미리 보기에서만 전체를 비트맵으로 그림.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAppearanceCustomization, NSAttributedStringAttachmentConveniences, NSAttributedStringNSStringDrawing, NSBaselineOffsetAttributeName,
    NSBitmapImageRep, NSColor, NSDeviceRGBColorSpace, NSFont, NSFontAttributeName, NSFontWeightMedium, NSFontWeightSemibold,
    NSForegroundColorAttributeName, NSGraphicsContext, NSImage, NSTextAttachment,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSAttributedString, NSDictionary, NSMutableAttributedString, NSNumber, NSString, NSUserDefaults};
use tiny_skia::{Color, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};
use tray_icon::TrayIcon;
use usagi_core::engine::ProviderId;
use usagi_core::logos;

/// 계정 하나의 칸
pub struct Part {
    pub provider: ProviderId,
    pub items: Vec<Item>,
    /// 그래프 대신 보일 글자 (로그아웃된 계정: "로그인 필요")
    pub note: Option<String>,
}

/// 그래프 하나: 짧은 라벨, 막대 (막대 표시를 끄면 없음), 값
#[derive(Debug)]
pub struct Item {
    pub label: String,
    pub bar: Option<Bar>,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    /// 채운 비율 0..1 (남은 양 기준이면 남은 비율)
    pub fill: f64,
    /// 색 단계 (texts::level): 0 여유, 1 주의, 2 위험
    pub level: u8,
}

/// 막대 색: Windows 위젯과 같은 여유/주의/위험
const BAR_COLORS: [(u8, u8, u8); 3] = [(52, 199, 89), (255, 176, 32), (255, 69, 58)];
/// 막대 크기 (pt). 메뉴 막대는 자리가 좁아서 Windows 위젯보다 짧게
const BAR_W: f64 = 24.0;
const BAR_H: f64 = 4.0;
/// 미리 보기 그림: 메뉴 막대 높이, 좌우 여백 (pt), 배율
const STRIP_H: f64 = 24.0;
const STRIP_PAD: f64 = 10.0;
const SCALE: f64 = 2.0;

/// 위젯 라벨 → 메뉴 막대용 짧은 라벨 (Windows 위젯의 아주 좁은 모양과 같음)
pub fn short_label(label: &str) -> String {
    match label {
        "5시간" => "5h".into(),
        "주간" => "7d".into(),
        other => {
            let name = other.strip_suffix(" 주간").unwrap_or(other);
            format!("{}7d", name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default())
        }
    }
}

/// 글자 색. 메뉴 막대는 시스템 색 (막대 밝기에 맞춰 바뀜), 미리 보기 그림은 정해진 색
struct Ink {
    label: Retained<NSColor>,
    value: Retained<NSColor>,
    divider: Retained<NSColor>,
    /// 로그인 필요 (Windows 위젯·설정 창과 같은 빨강)
    alert: Retained<NSColor>,
}

impl Ink {
    fn system() -> Self {
        Ink {
            label: NSColor::secondaryLabelColor(),
            value: NSColor::labelColor(),
            divider: NSColor::tertiaryLabelColor(),
            alert: NSColor::systemRedColor(),
        }
    }

    /// 시스템 색과 같은 밝기 (어두우면 흰색, 밝으면 검은색에 투명도)
    fn fixed(dark: bool) -> Self {
        let w = if dark { 1.0 } else { 0.0 };
        Ink {
            label: NSColor::colorWithWhite_alpha(w, if dark { 0.55 } else { 0.5 }),
            value: NSColor::colorWithWhite_alpha(w, 0.85),
            divider: NSColor::colorWithWhite_alpha(w, 0.25),
            alert: NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 69.0 / 255.0, 58.0 / 255.0, 1.0),
        }
    }
}

fn styled(text: &str, font: &NSFont, color: &NSColor, baseline: f64) -> Retained<NSAttributedString> {
    let base = NSNumber::new_f64(baseline);
    let keys: [&NSString; 3] = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSBaselineOffsetAttributeName] };
    let vals: [&AnyObject; 3] = [font.as_ref(), color.as_ref(), base.as_ref()];
    let dict = NSDictionary::from_slices(&keys, &vals);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(text), Some(&dict)) }
}

/// 그림 → 글자 사이에 넣는 첨부 (크기·위치는 pt, y는 글자 기준선에서)
fn attachment(pm: &Pixmap, w: f64, h: f64, y: f64) -> Option<Retained<NSAttributedString>> {
    let pixel_width = pm.width() as isize;
    let rep = rgba_bitmap(pixel_width, pm.height() as isize, pixel_width * 4)?;
    let bitmap = rep.bitmapData();
    if bitmap.is_null() {
        return None;
    }
    // Both the pixmap and this packed AppKit bitmap use premultiplied RGBA.
    let pixels = pm.data();
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), bitmap, pixels.len()) };
    let size = CGSize::new(w, h);
    rep.setSize(size);
    let img = NSImage::initWithSize(NSImage::alloc(), size);
    img.addRepresentation(&rep);
    let att = NSTextAttachment::new();
    att.setImage(Some(&img));
    att.setBounds(CGRect::new(CGPoint::new(0.0, y), CGSize::new(w, h)));
    Some(NSAttributedString::attributedStringWithAttachment(&att))
}

/// 공급자 로고 (Claude는 브랜드 색, OpenAI는 메뉴 막대 글자색)
fn logo(provider: ProviderId, dark: bool) -> Option<Retained<NSAttributedString>> {
    const PX: u32 = 28; // 14pt @2x
    let mask = logos::render(provider, PX);
    let (r, g, b) = logos::brand_color(provider).unwrap_or(if dark { (245, 245, 245) } else { (25, 25, 25) });
    let mut px = Vec::with_capacity((PX * PX * 4) as usize);
    for &a in &mask {
        let a32 = a as u32;
        px.extend_from_slice(&[(r as u32 * a32 / 255) as u8, (g as u32 * a32 / 255) as u8, (b as u32 * a32 / 255) as u8, a]);
    }
    let pm = Pixmap::from_vec(px, tiny_skia::IntSize::from_wh(PX, PX)?)?;
    attachment(&pm, 14.0, 14.0, -2.5)
}

/// 둥근 막대 그림 (바탕 + 채운 만큼 단계 색). 숫자 가운데 높이에 맞춤
fn bar(b: Bar, dark: bool) -> Option<Retained<NSAttributedString>> {
    let (w, h) = ((BAR_W * SCALE) as u32, (BAR_H * SCALE) as u32);
    let mut pm = Pixmap::new(w, h)?;
    let r = h as f32 / 2.0;
    let stroke = Stroke { width: h as f32, line_cap: LineCap::Round, ..Stroke::default() };
    let mut line = |end: f32, color: Color| {
        let mut pb = PathBuilder::new();
        pb.move_to(r, r);
        pb.line_to(end.max(r + 0.01), r);
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(color);
            paint.anti_alias = true;
            pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    };
    line(w as f32 - r, if dark { Color::from_rgba8(255, 255, 255, 70) } else { Color::from_rgba8(0, 0, 0, 55) });
    if b.fill > 0.0 {
        let fill_w = ((w as f64 * b.fill.min(1.0)).round() as f32).max(h as f32);
        let (cr, cg, cb) = BAR_COLORS[b.level.min(2) as usize];
        line(fill_w - r, Color::from_rgba8(cr, cg, cb, 255));
    }
    attachment(&pm, BAR_W, BAR_H, 2.5)
}

/// 메뉴 막대 글자 전체
fn compose(parts: &[Part], dark: bool, ink: &Ink) -> Retained<NSMutableAttributedString> {
    let label_font = NSFont::systemFontOfSize_weight(11.0, unsafe { NSFontWeightMedium });
    let value_font = NSFont::monospacedDigitSystemFontOfSize_weight(13.0, unsafe { NSFontWeightSemibold });
    let divider_font = NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightMedium });

    let out = NSMutableAttributedString::new();
    let space = |w: &str| styled(w, &label_font, &ink.label, 0.0);
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            out.appendAttributedString(&space(" "));
        } else {
            // 계정 사이 구분선 (Windows 위젯의 세로선과 같은 역할)
            out.appendAttributedString(&space("  "));
            out.appendAttributedString(&styled("│", &divider_font, &ink.divider, 0.0));
            out.appendAttributedString(&space("  "));
        }
        if let Some(l) = logo(part.provider, dark) {
            out.appendAttributedString(&l);
            out.appendAttributedString(&space(" "));
        }
        if let Some(note) = &part.note {
            out.appendAttributedString(&styled(note, &label_font, &ink.alert, 0.5));
            continue;
        }
        for (j, item) in part.items.iter().enumerate() {
            if j > 0 {
                out.appendAttributedString(&space("  "));
            }
            out.appendAttributedString(&styled(&format!("{} ", item.label), &label_font, &ink.label, 0.5));
            if let Some(b) = item.bar.and_then(|b| bar(b, dark)) {
                out.appendAttributedString(&b);
                out.appendAttributedString(&space(" "));
            }
            out.appendAttributedString(&styled(&item.value, &value_font, &ink.value, 0.0));
        }
    }
    if parts.is_empty() {
        // 계정이 없을 때: 눌러서 설정을 열 수 있게 이름을 보여 줌
        out.appendAttributedString(&styled(" usAgI", &value_font, &ink.value, 0.0));
    }
    out
}

/// 메뉴 막대 버튼에 꾸민 글자를 넣음.
pub fn apply(tray: &TrayIcon, parts: &[Part], dark: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(item) = tray.ns_status_item() else { return };
    let Some(button) = item.button(mtm) else { return };
    button.setAttributedTitle(&compose(parts, dark, &Ink::system()));
    // tray-icon's mouse view must follow AppKit's new title width.
    let _ = tray.set_tooltip(Some("usAgI"));
}

/// 설정 창 미리 보기 그림 (실제 픽셀 크기, RGBA). 띠 색은 설정 창의 미리 보기 띠와 같음
pub struct Preview {
    pub w: u32,
    pub h: u32,
    pub scale: f64,
    pub rgba: Vec<u8>,
}

fn rgba_bitmap(w: isize, h: isize, stride: isize) -> Option<Retained<NSBitmapImageRep>> {
    unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w,
            h,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            stride,
            0,
        )
    }
}

pub fn preview(parts: &[Part], dark: bool) -> Option<Preview> {
    MainThreadMarker::new()?;
    let text = compose(parts, dark, &Ink::fixed(dark));
    let text_size = text.size();
    let size = CGSize::new((text_size.width + 2.0 * STRIP_PAD).ceil(), STRIP_H);
    let (w, h) = ((size.width * SCALE) as isize, (size.height * SCALE) as isize);
    let rep = rgba_bitmap(w, h, 0)?;
    rep.setSize(size);
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    text.drawAtPoint(CGPoint::new(STRIP_PAD, ((size.height - text_size.height) / 2.0).round()));
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();

    // 투명 바탕에 그린 글자(미리 곱한 알파)를 띠 색 위에 얹음
    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    let stride = rep.bytesPerRow() as usize;
    let (w, h) = (rep.pixelsWide() as usize, rep.pixelsHigh() as usize);
    let bg: [u32; 3] = if dark { [32, 32, 32] } else { [238, 238, 238] };
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = unsafe { std::slice::from_raw_parts(data.add(y * stride), w * 4) };
        for px in row.chunks_exact(4) {
            let inv = 255 - px[3] as u32;
            for c in 0..3 {
                rgba.push((px[c] as u32 + bg[c] * inv / 255).min(255) as u8);
            }
            rgba.push(255);
        }
    }
    Some(Preview { w: w as u32, h: h as u32, scale: SCALE, rgba })
}

/// 시스템이 다크 모드인지 (버튼 모양은 화면에 붙기 전엔 밝게 나올 수 있어서 같이 봄)
fn system_dark() -> bool {
    NSUserDefaults::standardUserDefaults()
        .stringForKey(&NSString::from_str("AppleInterfaceStyle"))
        .is_some_and(|s| s.to_string().eq_ignore_ascii_case("dark"))
}

/// 메뉴 막대 항목의 눌림 표시 끄기. tray-icon은 오른쪽 버튼을 누를 때 켜기만 하고 끄지 않음
pub fn unhighlight(tray: &TrayIcon) {
    if let Some(button) = MainThreadMarker::new().and_then(|mtm| tray.ns_status_item().and_then(|i| i.button(mtm))) {
        button.highlight(false);
    }
}

/// 지금 메뉴 막대가 어두운지 (로고 색을 바꿀 때)
pub fn is_dark(tray: &TrayIcon) -> bool {
    system_dark()
        || MainThreadMarker::new()
            .and_then(|mtm| tray.ns_status_item().and_then(|i| i.button(mtm)))
            .is_some_and(|b| b.effectiveAppearance().name().to_string().contains("Dark"))
}
