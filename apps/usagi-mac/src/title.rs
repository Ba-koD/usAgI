//! 메뉴 막대 글자 꾸미기: [로고] 5h 53%  7d 64%   [로고] 7d 92%
//!
//! tray-icon의 글자는 꾸밀 수 없어서, 바탕이 되는 macOS NSStatusItem 버튼에 꾸민 글자(attributed title)를
//! 직접 넣어요. 글자는 시스템이 그려서 어떤 배율에서도 또렷하고, 밝은·어두운 메뉴 막대에 맞춰 색이 바뀌어요.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAppearanceCustomization, NSAttributedStringAttachmentConveniences, NSBaselineOffsetAttributeName, NSColor, NSFont,
    NSFontAttributeName, NSFontWeightMedium, NSFontWeightSemibold, NSForegroundColorAttributeName, NSImage, NSTextAttachment,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSAttributedString, NSData, NSDictionary, NSMutableAttributedString, NSNumber, NSString, NSUserDefaults};
use tray_icon::TrayIcon;
use usagi_core::engine::ProviderId;
use usagi_core::logos;

/// 계정 하나의 칸
pub struct Part {
    pub provider: ProviderId,
    /// (짧은 라벨, 값)
    pub items: Vec<(String, String)>,
}

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

fn styled(text: &str, font: &NSFont, color: &NSColor, baseline: f64) -> Retained<NSAttributedString> {
    let base = NSNumber::new_f64(baseline);
    let keys: [&NSString; 3] = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSBaselineOffsetAttributeName] };
    let vals: [&AnyObject; 3] = [font.as_ref(), color.as_ref(), base.as_ref()];
    let dict = NSDictionary::from_slices(&keys, &vals);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(text), Some(&dict)) }
}

/// 공급자 로고 (Claude는 브랜드 색, OpenAI는 메뉴 막대 글자색) → 글자 사이에 넣는 그림
fn logo(provider: ProviderId, dark: bool) -> Option<Retained<NSAttributedString>> {
    const PX: u32 = 28; // 14pt @2x
    let mask = logos::render(provider, PX);
    let (r, g, b) = logos::brand_color(provider).unwrap_or(if dark { (245, 245, 245) } else { (25, 25, 25) });
    let mut px = Vec::with_capacity((PX * PX * 4) as usize);
    for &a in &mask {
        let a32 = a as u32;
        px.extend_from_slice(&[(r as u32 * a32 / 255) as u8, (g as u32 * a32 / 255) as u8, (b as u32 * a32 / 255) as u8, a]);
    }
    let pm = tiny_skia::Pixmap::from_vec(px, tiny_skia::IntSize::from_wh(PX, PX)?)?;
    let png = pm.encode_png().ok()?;
    let img = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&png))?;
    img.setSize(CGSize::new(14.0, 14.0));
    let att = NSTextAttachment::new();
    att.setImage(Some(&img));
    att.setBounds(CGRect::new(CGPoint::new(0.0, -2.5), CGSize::new(14.0, 14.0)));
    Some(NSAttributedString::attributedStringWithAttachment(&att))
}

/// 메뉴 막대 버튼에 꾸민 글자를 넣음
pub fn apply(tray: &TrayIcon, parts: &[Part]) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(item) = tray.ns_status_item() else { return };
    let Some(button) = item.button(mtm) else { return };
    let dark = system_dark() || button.effectiveAppearance().name().to_string().contains("Dark");

    let label_font = NSFont::systemFontOfSize_weight(11.0, unsafe { NSFontWeightMedium });
    let value_font = NSFont::monospacedDigitSystemFontOfSize_weight(13.0, unsafe { NSFontWeightSemibold });
    let label_color = NSColor::secondaryLabelColor();
    let value_color = NSColor::labelColor();
    let divider_color = NSColor::tertiaryLabelColor();
    let divider_font = NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightMedium });

    let out = NSMutableAttributedString::new();
    let space = |w: &str| styled(w, &label_font, &label_color, 0.0);
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            out.appendAttributedString(&space(" "));
        } else {
            // 계정 사이 구분선 (Windows 위젯의 세로선과 같은 역할)
            out.appendAttributedString(&space("  "));
            out.appendAttributedString(&styled("│", &divider_font, &divider_color, 0.0));
            out.appendAttributedString(&space("  "));
        }
        if let Some(l) = logo(part.provider, dark) {
            out.appendAttributedString(&l);
            out.appendAttributedString(&space(" "));
        }
        for (j, (label, value)) in part.items.iter().enumerate() {
            if j > 0 {
                out.appendAttributedString(&space("  "));
            }
            out.appendAttributedString(&styled(&format!("{label} "), &label_font, &label_color, 0.5));
            out.appendAttributedString(&styled(value, &value_font, &value_color, 0.0));
        }
    }
    if parts.is_empty() {
        // 계정이 없을 때: 눌러서 설정을 열 수 있게 이름을 보여 줌
        out.appendAttributedString(&styled(" usAgI", &value_font, &value_color, 0.0));
    }
    button.setAttributedTitle(&out);
    // 글자 너비가 바뀌면 누르는 영역도 다시 맞춤 (tray-icon은 툴팁을 바꿀 때 영역을 다시 잼)
    let _ = tray.set_tooltip(Some("usAgI"));
}

/// 시스템이 다크 모드인지 (버튼 모양은 화면에 붙기 전엔 밝게 나올 수 있어서 같이 봄)
fn system_dark() -> bool {
    NSUserDefaults::standardUserDefaults()
        .stringForKey(&NSString::from_str("AppleInterfaceStyle"))
        .is_some_and(|s| s.to_string().eq_ignore_ascii_case("dark"))
}

/// 지금 메뉴 막대가 어두운지 (로고 색을 바꿀 때)
pub fn is_dark(tray: &TrayIcon) -> bool {
    system_dark()
        || MainThreadMarker::new()
            .and_then(|mtm| tray.ns_status_item().and_then(|i| i.button(mtm)))
            .is_some_and(|b| b.effectiveAppearance().name().to_string().contains("Dark"))
}
