fn main() {
    // 화면 문구는 한국어가 원문, 영어는 lang/en/LC_MESSAGES/usagi-settings.po
    println!("cargo:rerun-if-changed=lang");
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_bundled_translations("lang");
    let source = if cfg!(feature = "external-fonts")
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
    {
        "ui/settings.slint"
    } else {
        "ui/settings-embedded.slint"
    };
    slint_build::compile_with_config(source, config).expect("compile settings UI");
}
