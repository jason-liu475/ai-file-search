use ai_file_search_core::PathId;

#[test]
fn normalizes_redundant_current_directory_segments() {
    let path = PathId::from_user_path("./Documents/./Report.txt");

    assert_eq!(path.as_normalized(), "Documents/Report.txt");
}

#[test]
fn normalizes_windows_separators_to_forward_slashes() {
    let path = PathId::from_user_path("Documents\\Invoices\\June.pdf");

    assert_eq!(path.as_normalized(), "Documents/Invoices/June.pdf");
}
