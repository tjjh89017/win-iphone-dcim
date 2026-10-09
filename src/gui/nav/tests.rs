use super::*;

const DCIM: &str = "/Internal Storage/DCIM";
const A: &str = "/Internal Storage/DCIM/202601_a";

#[test]
fn going_somewhere_new_clears_forward() {
    let mut h = NavHistory::default();
    h.reset("/");
    h.go(DCIM);
    h.go(A);
    assert_eq!(h.back(), Some(DCIM));
    assert!(h.can_forward());
    h.go("/Internal Storage");
    assert!(!h.can_forward());
    assert_eq!(h.back(), Some(DCIM));
    assert_eq!(h.back(), Some("/"));
    assert_eq!(h.back(), None);
    assert_eq!(h.current(), Some("/"));
}

#[test]
fn back_and_forward_round_trip() {
    let mut h = NavHistory::default();
    h.reset("/");
    h.go(DCIM);
    h.go(A);
    // Going to the shown folder adds no history.
    h.go(A);
    assert_eq!(h.back(), Some(DCIM));
    assert_eq!(h.back(), Some("/"));
    assert!(!h.can_back());
    assert_eq!(h.forward(), Some(DCIM));
    assert_eq!(h.forward(), Some(A));
    assert_eq!(h.forward(), None);
    assert_eq!(h.current(), Some(A));
}

#[test]
fn up_pushes_history_and_stops_at_the_root() {
    let mut h = NavHistory::default();
    h.reset(A);
    assert!(h.can_up());
    assert_eq!(h.up(), Some(DCIM));
    assert_eq!(h.up(), Some("/Internal Storage"));
    assert_eq!(h.up(), Some("/"));
    assert!(!h.can_up());
    assert_eq!(h.up(), None);
    assert_eq!(h.current(), Some("/"));
    assert_eq!(h.back(), Some("/Internal Storage"));
    // A reset forgets the history.
    h.reset("/");
    assert!(!h.can_back() && !h.can_forward());
}

#[test]
fn breadcrumb_segments() {
    assert_eq!(
        breadcrumbs(A),
        [
            ("/".to_owned(), "/".to_owned()),
            (
                "Internal Storage".to_owned(),
                "/Internal Storage".to_owned()
            ),
            ("DCIM".to_owned(), DCIM.to_owned()),
            ("202601_a".to_owned(), A.to_owned()),
        ]
    );
    assert_eq!(breadcrumbs("/"), [("/".to_owned(), "/".to_owned())]);
}
