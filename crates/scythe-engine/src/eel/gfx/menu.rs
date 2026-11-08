//! `gfx_showmenu` strings: items split at `|`, each led by any of `!` (checked), `#`
//! (greyed), `>` (opens a submenu named by the item) and `<` (the submenu's last item). An
//! empty item is a separator. Choosable items are numbered from 1 in order.

#[derive(Debug, Clone, PartialEq)]
pub enum MenuItem {
    Item { label: String, id: u32, checked: bool, enabled: bool },
    Sub { label: String, items: Vec<MenuItem>, enabled: bool },
    Separator,
}

/// A menu `@gfx` asked for, at `(x, y)` in the view.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub items: Vec<MenuItem>,
    pub x: f64,
    pub y: f64,
}

pub fn parse(text: &str) -> Vec<MenuItem> {
    // Open submenus: their label, whether greyed, and their items so far.
    let mut stack: Vec<(String, bool, Vec<MenuItem>)> = vec![(String::new(), true, Vec::new())];
    let mut next = 1;
    for raw in text.split('|') {
        let (mut checked, mut greyed, mut opens, mut closes) = (false, false, false, false);
        let mut label = raw;
        while let Some(c) = label.chars().next().filter(|c| "!#><".contains(*c)) {
            match c {
                '!' => checked = true,
                '#' => greyed = true,
                '>' => opens = true,
                _ => closes = true,
            }
            label = &label[1..];
        }
        let items = &mut stack.last_mut().expect("the top menu stays").2;
        if opens {
            stack.push((label.to_owned(), !greyed, Vec::new()));
            continue;
        }
        if !label.is_empty() {
            items.push(MenuItem::Item { label: label.to_owned(), id: next, checked, enabled: !greyed });
            next += 1;
        } else if !closes {
            items.push(MenuItem::Separator);
        }
        if closes && stack.len() > 1 {
            let (label, enabled, items) = stack.pop().expect("more than one");
            stack.last_mut().expect("the top menu stays").2.push(MenuItem::Sub { label, items, enabled });
        }
    }
    while stack.len() > 1 {
        let (label, enabled, items) = stack.pop().expect("more than one");
        stack.last_mut().expect("the top menu stays").2.push(MenuItem::Sub { label, items, enabled });
    }
    stack.pop().map(|s| s.2).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, id: u32, checked: bool, enabled: bool) -> MenuItem {
        MenuItem::Item { label: label.into(), id, checked, enabled }
    }

    #[test]
    fn numbers_choosable_items_across_submenus() {
        let menu = parse("first||!second|>third|#in sub|<last in sub|fourth|>Window|!a|b|<");
        assert_eq!(
            menu,
            [
                item("first", 1, false, true),
                MenuItem::Separator,
                item("second", 2, true, true),
                MenuItem::Sub {
                    label: "third".into(),
                    items: vec![item("in sub", 3, false, false), item("last in sub", 4, false, true)],
                    enabled: true,
                },
                item("fourth", 5, false, true),
                MenuItem::Sub {
                    label: "Window".into(),
                    items: vec![item("a", 6, true, true), item("b", 7, false, true)],
                    enabled: true,
                },
            ]
        );
    }
}
