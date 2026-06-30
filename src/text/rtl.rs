//! Right-to-Left (RTL) text delegation to the main rtl_text module.

pub use crate::rtl_text::{
    contains_rtl, create_layout_job_for_comment, create_layout_job_for_line,
    find_comment_start, is_rtl_char, split_at_comment, create_layout_job_for_code,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_rtl_char_arabic() {
        assert!(is_rtl_char('ا')); // Arabic letter Alef
        assert!(is_rtl_char('ب')); // Arabic letter Beh
        assert!(is_rtl_char('ت')); // Arabic letter Teh
        assert!(is_rtl_char('ـ')); // Arabic tatweel
        assert!(is_rtl_char('\u{06FF}')); // End of Arabic range
    }

    #[test]
    fn test_is_rtl_char_hebrew() {
        assert!(is_rtl_char('א')); // Hebrew letter Alef
        assert!(is_rtl_char('ב')); // Hebrew letter Bet
        assert!(is_rtl_char('ת')); // Hebrew letter Tav
        assert!(is_rtl_char('\u{05FF}')); // End of Hebrew range
    }

    #[test]
    fn test_is_rtl_char_latin() {
        assert!(!is_rtl_char('a'));
        assert!(!is_rtl_char('Z'));
        assert!(!is_rtl_char('0'));
        assert!(!is_rtl_char(' '));
        assert!(!is_rtl_char('='));
    }

    #[test]
    fn test_contains_rtl() {
        assert!(contains_rtl("hello عالم"));
        assert!(contains_rtl("שלום world"));
        assert!(contains_rtl("// comment in עברית"));
        assert!(!contains_rtl("hello world"));
        assert!(!contains_rtl("// regular comment"));
        assert!(!contains_rtl(""));
    }

    #[test]
    fn test_find_comment_start() {
        assert_eq!(find_comment_start("code // comment"), Some(5));
        assert_eq!(find_comment_start("code /* comment"), Some(5));
        assert_eq!(find_comment_start("// comment only"), Some(0));
        assert_eq!(find_comment_start("/* comment only"), Some(0));
        assert_eq!(find_comment_start("no comment here"), None);
        assert_eq!(find_comment_start(""), None);
    }

    #[test]
    fn test_split_at_comment() {
        assert_eq!(
            split_at_comment("code // comment"),
            Some(("code ", "// comment"))
        );
        assert_eq!(
            split_at_comment("// comment only"),
            Some(("", "// comment only"))
        );
        assert_eq!(
            split_at_comment("code /* comment"),
            Some(("code ", "/* comment"))
        );
        assert_eq!(split_at_comment("no comment"), None);
    }
}
