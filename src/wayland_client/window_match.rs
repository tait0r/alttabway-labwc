/// One side of a toplevel that should be paired with the other protocol's list.
///
/// `ready` is false until the compositor has committed title and app id.
/// Not-ready windows still occupy a slot so a later window with the same
/// app id and title cannot take their capture source.
pub(super) struct MatchWindow<'a, T> {
    pub app_id: &'a str,
    pub title: &'a str,
    pub ready: bool,
    pub data: T,
}

/// Pair zwlr toplevels (focus) with ext toplevels (capture).
///
/// Windows are grouped by app id and title. Inside a group, the nth focus
/// window pairs with the nth capture window, in list order. Two windows that
/// share an app id and title stay distinct, and a capture source with no focus
/// window is left over rather than becoming a second entry.
pub(super) fn pair_windows<A: Copy, B: Copy>(
    left: &[MatchWindow<'_, A>],
    right: &[MatchWindow<'_, B>],
) -> Vec<(A, B)> {
    let mut pairs = Vec::new();

    for (left_index, left_window) in left.iter().enumerate() {
        let group_index = left[..left_index]
            .iter()
            .filter(|window| {
                window.app_id == left_window.app_id && window.title == left_window.title
            })
            .count();

        let Some(right_window) = right
            .iter()
            .filter(|window| {
                window.app_id == left_window.app_id && window.title == left_window.title
            })
            .nth(group_index)
        else {
            continue;
        };

        if left_window.ready && right_window.ready {
            pairs.push((left_window.data, right_window.data));
        }
    }

    pairs
}

#[cfg(test)]
mod tests {
    use super::{MatchWindow, pair_windows};

    fn window<'a, T>(
        app_id: &'a str,
        title: &'a str,
        ready: bool,
        data: T,
    ) -> MatchWindow<'a, T> {
        MatchWindow {
            app_id,
            title,
            ready,
            data,
        }
    }

    #[test]
    fn distinct_windows_pair_across_different_list_order() {
        let left = [
            window("firefox", "Inbox", true, 1),
            window("foot", "terminal", true, 2),
        ];
        let right = [
            window("foot", "terminal", true, 20),
            window("firefox", "Inbox", true, 10),
        ];

        assert_eq!(pair_windows(&left, &right), vec![(1, 10), (2, 20)]);
    }

    #[test]
    fn identical_app_id_and_title_stay_separate() {
        let left = [
            window("firefox", "Mozilla Firefox", true, 1),
            window("firefox", "Mozilla Firefox", true, 2),
        ];
        let right = [
            window("firefox", "Mozilla Firefox", true, 10),
            window("firefox", "Mozilla Firefox", true, 20),
        ];

        assert_eq!(pair_windows(&left, &right), vec![(1, 10), (2, 20)]);
    }

    #[test]
    fn extra_capture_source_does_not_create_another_pair() {
        let left = [window("firefox", "Mozilla Firefox", true, 1)];
        let right = [
            window("firefox", "Mozilla Firefox", true, 10),
            window("firefox", "Mozilla Firefox", true, 20),
        ];

        assert_eq!(pair_windows(&left, &right), vec![(1, 10)]);
    }

    #[test]
    fn extra_focus_window_stays_unpaired() {
        let left = [
            window("firefox", "Mozilla Firefox", true, 1),
            window("firefox", "Mozilla Firefox", true, 2),
        ];
        let right = [window("firefox", "Mozilla Firefox", true, 10)];

        assert_eq!(pair_windows(&left, &right), vec![(1, 10)]);
    }

    #[test]
    fn same_title_with_different_app_ids_do_not_pair() {
        let left = [
            window("firefox", "Inbox", true, 1),
            window("thunderbird", "Inbox", true, 2),
        ];
        let right = [
            window("thunderbird", "Inbox", true, 20),
            window("firefox", "Inbox", true, 10),
        ];

        assert_eq!(pair_windows(&left, &right), vec![(1, 10), (2, 20)]);
    }

    #[test]
    fn not_ready_window_keeps_its_slot() {
        let left = [
            window("firefox", "Mozilla Firefox", false, 1),
            window("firefox", "Mozilla Firefox", true, 2),
        ];
        let right = [
            window("firefox", "Mozilla Firefox", true, 10),
            window("firefox", "Mozilla Firefox", true, 20),
        ];

        assert_eq!(pair_windows(&left, &right), vec![(2, 20)]);
    }
}
