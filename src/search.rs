//! What the search palette finds for a query, and in what order.
//!
//! Pure: the shell hands in page titles and settings, this hands back
//! which match and how well, and nothing here knows what a `Screen` or a
//! `Message` is. That is what lets the ordering be tested without a
//! window.

/// How well `text` matches `query`, lower being better, or `None` for no
/// match at all.
///
/// Three tiers, because a palette's first result is what Enter picks and
/// it has to be the obvious one: typing `blur` should put "Blur" before
/// "Window blur" before "Motion blur passes behind…". Case never matters.
///
/// - 0: `text` starts with the query
/// - 1: a word inside `text` starts with it — a word being anything after
///   a space, a colon, an underscore, a hyphen, a dot or a slash, so
///   `blur` finds `decoration:blur:passes` at the `blur` after a colon
/// - 2: the query appears anywhere
///
/// An empty query matches nothing: an empty palette is closed, not a
/// list of everything.
pub fn score(query: &str, text: &str) -> Option<u8> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }
    let text = text.to_lowercase();
    if text.starts_with(&query) {
        return Some(0);
    }
    let at_word = text
        .match_indices(&query)
        .any(|(i, _)| text[..i].ends_with([' ', ':', '_', '-', '.', '/']));
    if at_word {
        return Some(1);
    }
    text.contains(&query).then_some(2)
}

/// The items of `items` that match `query` on `text_of`, best first, at
/// most `limit` of them.
///
/// Stable within a tier, so items that match equally well keep the
/// order they were given in — the page's own order, which is the order a
/// user has already seen them in.
pub fn best<'a, T>(
    query: &str,
    items: impl IntoIterator<Item = &'a T>,
    text_of: impl Fn(&T) -> &str,
    limit: usize,
) -> Vec<&'a T>
where
    T: 'a,
{
    let mut hits: Vec<(u8, usize, &'a T)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(i, item)| score(query, text_of(item)).map(|s| (s, i, item)))
        .collect();
    hits.sort_by_key(|(s, i, _)| (*s, *i));
    hits.into_iter().take(limit).map(|(_, _, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Enter takes the first result, so the first result has to be the
    /// one that starts with what was typed.
    #[test]
    fn a_name_that_starts_with_the_query_comes_first() {
        // One of each tier, given worst first: a bare substring, a word
        // start, and a prefix.
        let names = ["Motionblur passes", "Window blur", "Blur"];
        let found = best("blur", names.iter(), |s| s, 10);
        assert_eq!(found, [&"Blur", &"Window blur", &"Motionblur passes"]);
    }

    /// Config keys are colon-separated words, and a search for the last
    /// word must find it at the colon rather than rank it as a mere
    /// substring.
    #[test]
    fn a_key_matches_at_each_of_its_colon_separated_words() {
        assert_eq!(score("blur", "decoration:blur:passes"), Some(1));
        assert_eq!(score("pass", "decoration:blur:passes"), Some(1));
        assert_eq!(score("ecor", "decoration:blur"), Some(2));
    }

    #[test]
    fn case_never_matters() {
        assert_eq!(score("BLUR", "Window blur"), Some(1));
        assert_eq!(score("window", "Window blur"), Some(0));
    }

    /// An empty palette is closed. Matching everything on an empty query
    /// would open it full the moment the field is focused.
    #[test]
    fn an_empty_query_matches_nothing() {
        assert_eq!(score("", "anything"), None);
        assert_eq!(score("   ", "anything"), None);
        assert!(best("", ["a", "b"].iter(), |s| s, 10).is_empty());
    }

    /// Equal matches keep the page's order, and the limit is a limit.
    #[test]
    fn equal_matches_keep_their_order_and_the_limit_holds() {
        let names = ["gaps in", "gaps out", "gaps workspaces"];
        let found = best("gaps", names.iter(), |s| s, 2);
        assert_eq!(found, [&"gaps in", &"gaps out"]);
    }

    #[test]
    fn something_that_does_not_contain_the_query_is_not_found() {
        assert_eq!(score("blur", "shadow range"), None);
    }
}
