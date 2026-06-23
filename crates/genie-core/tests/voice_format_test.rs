#![cfg(feature = "voice")]
use genie_core::voice::format::for_voice;

#[test]
fn strip_bold_and_italic() {
    assert_eq!(
        for_voice("This is **bold** and *italic*"),
        "This is bold and italic"
    );
}

#[test]
fn strip_markdown_headers() {
    assert_eq!(for_voice("## Weather\nIt's sunny."), "Weather Its sunny.");
}

#[test]
fn strip_bullet_points() {
    let input = "Here's what I found:\n- Item one\n- Item two\n- Item three";
    let output = for_voice(input);
    assert!(output.contains("Item one"));
    assert!(!output.contains("- "));
}

#[test]
fn strip_code_blocks() {
    let input = "Here's the code:\n```\nlet x = 5;\n```\nThat's it.";
    let output = for_voice(input);
    assert!(!output.contains("let x"));
    assert!(output.contains("Thats it"));
}

#[test]
fn strip_links() {
    let input = "Check [this guide](https://example.com) for details.";
    let output = for_voice(input);
    assert!(output.contains("this guide"));
    assert!(!output.contains("https://"));
}

#[test]
fn strip_raw_urls_from_plain_text() {
    let input = "Top result: ESP32-C6 supports Thread. https://example.com/thread";
    let output = for_voice(input);
    assert!(output.contains("ESP32-C6 supports Thread"));
    assert!(!output.contains("https://"));
}

#[test]
fn truncate_long_response() {
    let input = "First sentence. Second sentence. Third sentence. Fourth sentence. Fifth sentence.";
    let output = for_voice(input);
    assert!(output.contains("First"));
    assert!(output.contains("Third"));
    assert!(!output.contains("Fourth"));
}

#[test]
fn truncate_handles_chinese_punctuation() {
    let input = "第一句。第二句！第三句？第四句。";
    let output = for_voice(input);
    assert!(output.contains("第一句"));
    assert!(output.contains("第三句"));
    assert!(!output.contains("第四句"));
}

#[test]
fn clean_special_chars() {
    let input = "The temperature is 72°F (about 22°C)...nice!";
    let output = for_voice(input);
    assert!(!output.contains("("));
    assert!(!output.contains("..."));
}

#[test]
fn empty_input() {
    assert_eq!(for_voice(""), "");
}

#[test]
fn already_clean() {
    assert_eq!(for_voice("The lights are on."), "The lights are on.");
}

#[test]
fn decimal_numbers_are_not_split() {
    let output = for_voice("It is 72.5 degrees outside and feels warm.");
    assert!(
        output.contains("72.5"),
        "decimal must stay intact, got {output:?}"
    );
    assert!(
        !output.contains("72. 5"),
        "decimal must not be split, got {output:?}"
    );
}

#[test]
fn version_strings_are_not_split() {
    let output = for_voice("Version v1.0.0 is installed and ready.");
    assert!(
        output.contains("v1.0.0"),
        "version dots must stay intact, got {output:?}"
    );
}

#[test]
fn abbreviations_do_not_end_a_sentence() {
    let output = for_voice("Dinner is at 6 p.m. today in the kitchen.");
    assert!(
        output.contains("p.m."),
        "abbreviation must stay intact, got {output:?}"
    );
    assert!(
        !output.contains("p. m."),
        "abbreviation must not be split, got {output:?}"
    );
    assert!(
        output.contains("today in the kitchen"),
        "clause after the abbreviation must remain, got {output:?}"
    );
}

#[test]
fn cap_counts_real_sentences_not_decimal_fragments() {
    let output = for_voice("The CPU is at 3.50 GHz now. Memory usage is fine. All systems normal.");
    assert!(output.contains("3.50 GHz"), "got {output:?}");
    assert!(output.contains("Memory usage is fine"), "got {output:?}");
    assert!(
        output.contains("All systems normal"),
        "the third real sentence must not be dropped, got {output:?}"
    );
}

#[test]
fn chinese_punctuation_still_segments_immediately() {
    let output = for_voice("第一句。第二句！第三句？第四句。");
    assert!(output.contains("第一句"));
    assert!(output.contains("第三句"));
    assert!(!output.contains("第四句"));
}
