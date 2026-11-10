use std::ffi::OsString;

use ferry_platform_windows::windows_command_line;

#[test]
fn command_line_quotes_each_argument() {
    assert_eq!(
        windows_command_line(&[OsString::from("hello world"), OsString::from("--autostart")]),
        "\"hello world\" \"--autostart\""
    );
}

#[test]
fn empty_arguments_stay_empty() {
    assert_eq!(windows_command_line(&[OsString::from("")]), "\"\"");
}

#[test]
fn trailing_backslashes_are_preserved_before_the_closing_quote() {
    assert_eq!(
        windows_command_line(&[OsString::from(r"C:\Program Files\ScreenFerry\")]),
        "\"C:\\Program Files\\ScreenFerry\\\\\""
    );
}

#[test]
fn embedded_quotes_are_escaped() {
    assert_eq!(
        windows_command_line(&[OsString::from("a\"b")]),
        "\"a\\\"b\""
    );
}

#[test]
fn a_data_directory_with_spaces_and_a_trailing_slash_is_safe() {
    let args = [
        OsString::from(r"C:\Program Files\ScreenFerry\screenferry.exe"),
        OsString::from("--data-dir"),
        OsString::from(r"D:\ScreenFerry data\"),
        OsString::from("--autostart"),
    ];
    assert_eq!(
        windows_command_line(&args),
        "\"C:\\Program Files\\ScreenFerry\\screenferry.exe\" \"--data-dir\" \
         \"D:\\ScreenFerry data\\\\\" \"--autostart\""
    );
}
