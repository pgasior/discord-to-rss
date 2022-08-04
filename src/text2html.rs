use linkify::LinkFinder;

pub fn text2html(text: &str) -> String {
    let finder = LinkFinder::new();
    let content = finder
        .spans(&ammonia::clean(text))
        .map(|span| match span.kind() {
            Some(linkify::LinkKind::Url | linkify::LinkKind::Email) => {
                format!("<a href=\"{0}\">{0}</a>", span.as_str())
            }
            Some(_) | None => span.as_str().to_string(),
        })
        .collect::<String>();
    format!("<pre>{}</pre>", &content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_links_and_pre() {
        assert_eq!(
            "<pre>text <a href=\"https://google.com\">https://google.com</a> text</pre>",
            text2html("text https://google.com text")
        );
    }

    #[test]
    fn sanitizes_emoji() {
        assert_eq!(
            "<pre>@everyone </pre>",
            text2html("@everyone <a:headpat:940745698587074570>")
        );
    }
}
