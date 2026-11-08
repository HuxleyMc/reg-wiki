//! Parsing the librarian's replies. Small models follow simple tags more reliably than
//! JSON, so replies are tagged sections; parsing is lenient about what's around them.

/// The text of `<tag>…</tag>`, or `None` if the tag isn't there. A missing closing tag
/// runs to the next opening tag of `others`, or the end.
pub fn section(text: &str, tag: &str, others: &[&str]) -> Option<String> {
    let open = format!("<{tag}>");
    let start = text.find(&open)? + open.len();
    let rest = &text[start..];
    let close = format!("</{tag}>");
    let end = rest.find(&close).unwrap_or_else(|| {
        others
            .iter()
            .filter_map(|o| rest.find(&format!("<{o}>")))
            .min()
            .unwrap_or(rest.len())
    });
    Some(rest[..end].trim().to_string())
}

/// Non-empty lines of a section, without list markers; "none" and the like are dropped.
pub fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['-', '*', '•'])
                .trim()
                .to_string()
        })
        .filter(|l| {
            let low = l.to_lowercase();
            !l.is_empty()
                && !matches!(
                    low.trim_end_matches('.'),
                    "none" | "n/a" | "(none)" | "nothing"
                )
        })
        .collect()
}

/// A slug from a line such as `myapp/billing — summary`, `[[myapp/billing]]` or
/// `` `myapp/billing` ``.
pub fn slug_of(line: &str) -> Option<String> {
    let l = line.trim();
    let l = l.split(" — ").next().unwrap_or(l);
    let l = l.split(" - ").next().unwrap_or(l);
    let l = l.split(": ").next().unwrap_or(l);
    let s = l
        .trim()
        .trim_matches(|c| matches!(c, '[' | ']' | '`' | '*' | '"' | '\'' | '(' | ')'))
        .trim();
    let s = s.split('|').next().unwrap_or(s);
    let s = s.split('#').next().unwrap_or(s).trim();
    (!s.is_empty() && !s.contains(char::is_whitespace)).then(|| s.to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub answer: String,
    pub sources: Vec<String>,
    pub gaps: Vec<String>,
    pub need: Vec<String>,
}

const ANSWER_TAGS: &[&str] = &["answer", "sources", "gaps", "need"];

/// `None` when there's no `<answer>` section with something in it.
pub fn answer(text: &str) -> Option<Answer> {
    let get = |t: &str| section(text, t, ANSWER_TAGS);
    let answer = get("answer").filter(|a| !a.is_empty())?;
    let slugs = |t: &str| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in lines(&get(t).unwrap_or_default())
            .iter()
            .filter_map(|l| slug_of(l))
        {
            if !out.contains(&s) {
                out.push(s);
            }
        }
        out
    };
    Some(Answer {
        answer,
        sources: slugs("sources"),
        gaps: lines(&get("gaps").unwrap_or_default()),
        need: slugs("need"),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub slug: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Edits {
    pub pages: Vec<Proposal>,
    pub deletes: Vec<String>,
    pub message: Option<String>,
    /// The reply said there's nothing to change (`<none/>`).
    pub none: bool,
}

/// `<page slug="…">…</page>` blocks, `<delete slug="…"/>` and `<message>…</message>`.
pub fn edits(text: &str) -> Edits {
    let mut out = Edits {
        message: section(text, "message", &["page"]).filter(|m| !m.is_empty()),
        ..Edits::default()
    };
    let mut rest = text;
    while let Some(i) = rest.find("<page") {
        let after = &rest[i + 5..];
        let Some(gt) = after.find('>') else { break };
        let attrs = &after[..gt];
        let body_start = &after[gt + 1..];
        let (body, next) = match body_start.find("</page>") {
            Some(j) => (&body_start[..j], &body_start[j + 7..]),
            None => match body_start.find("<page") {
                Some(j) => (&body_start[..j], &body_start[j..]),
                None => (body_start, ""),
            },
        };
        if let Some(slug) = attr(attrs, "slug") {
            out.pages.push(Proposal {
                slug,
                content: unfence(body),
            });
        }
        rest = next;
    }
    let mut rest = text;
    while let Some(i) = rest.find("<delete") {
        let after = &rest[i + 7..];
        let Some(gt) = after.find('>') else { break };
        if let Some(slug) = attr(&after[..gt], "slug")
            && !out.deletes.contains(&slug)
        {
            out.deletes.push(slug);
        }
        rest = &after[gt + 1..];
    }
    out.none = out.pages.is_empty() && out.deletes.is_empty() && text.contains("<none");
    out
}

fn attr(attrs: &str, name: &str) -> Option<String> {
    let i = attrs.find(&format!("{name}="))? + name.len() + 1;
    let rest = &attrs[i..];
    let q = rest.chars().next()?;
    let v = if q == '"' || q == '\'' {
        let r = &rest[1..];
        &r[..r.find(q)?]
    } else {
        rest.split(|c: char| c.is_whitespace() || c == '/').next()?
    };
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Page text, without a ```` ```markdown ```` fence a model may have wrapped it in, and
/// ending in exactly one newline.
fn unfence(body: &str) -> String {
    let t = body.trim_matches(|c| c == '\n' || c == '\r');
    let t = t.trim_start_matches([' ', '\t']);
    let inner = t
        .strip_prefix("```")
        .and_then(|r| r.split_once('\n'))
        .map(|(_, r)| r)
        .and_then(|r| r.trim_end().strip_suffix("```"));
    let mut s = inner.unwrap_or(t).trim_end().to_string();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers() {
        let r = "Sure.\n<answer>\nRetries are capped at 3.\n</answer>\n<sources>\n- myapp/billing — How billing works\n[[myapp/billing/stripe]]\n- myapp/billing\n</sources>\n<gaps>\nnone\n</gaps>\n<need>\n</need>";
        let a = answer(r).unwrap();
        assert_eq!(a.answer, "Retries are capped at 3.");
        assert_eq!(a.sources, ["myapp/billing", "myapp/billing/stripe"]);
        assert!(a.gaps.is_empty() && a.need.is_empty());
        // Unclosed tags run to the next one.
        let a = answer("<answer>yes\n<sources>a/b\n<gaps>- how X works").unwrap();
        assert_eq!(a.answer, "yes");
        assert_eq!(a.sources, ["a/b"]);
        assert_eq!(a.gaps, ["how X works"]);
        assert!(answer("no tags here").is_none());
        assert!(answer("<answer>\n</answer>").is_none());
    }

    #[test]
    fn slugs_from_lines() {
        assert_eq!(slug_of("`x/y` - why").as_deref(), Some("x/y"));
        assert_eq!(slug_of("[[x/y#Retries|r]]").as_deref(), Some("x/y"));
        assert_eq!(slug_of("x/y: because").as_deref(), Some("x/y"));
        assert_eq!(slug_of("two words"), None);
    }

    #[test]
    fn edit_blocks() {
        let r = "<page slug=\"myapp/a\">\n```markdown\n---\ntitle: A\n---\nbody\n```\n</page>\n\
                 <page slug='myapp/b'>\n---\ntitle: B\n---\n</page>\n<delete slug=\"myapp/c\"/>\n\
                 <message>Merge c into a</message>";
        let e = edits(r);
        assert_eq!(e.pages.len(), 2);
        assert_eq!(e.pages[0].slug, "myapp/a");
        assert_eq!(e.pages[0].content, "---\ntitle: A\n---\nbody\n");
        assert_eq!(e.pages[1].content, "---\ntitle: B\n---\n");
        assert_eq!(e.deletes, ["myapp/c"]);
        assert_eq!(e.message.as_deref(), Some("Merge c into a"));
        assert!(!e.none);
        assert!(edits("<none/>").none);
        assert!(edits("whatever").pages.is_empty());
    }
}
