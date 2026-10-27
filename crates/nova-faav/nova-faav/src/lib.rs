//! Process-wide get/set cells, in the style of TAM's `faav.rs`.
//!
//! One function per cell: `Some(x)` stores and returns `x`, `None` just
//! returns what is stored. No lock to deadlock or poison — a cell is a plain
//! `static mut` reached through raw pointers, which is also why the reader
//! and the writer are expected to be different moments of one flow, not two
//! threads racing the same cell.

/// The Search box's current text. `None` while no filter is typed.
pub fn search(query: Option<String>) -> Option<String> {
    static mut STATE: String = String::new();
    unsafe {
        if let Some(x) = query {
            *std::ptr::addr_of_mut!(STATE) = x;
        }
        let current = (*std::ptr::addr_of!(STATE)).clone();
        if current.is_empty() {
            return None;
        }
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::search;

    #[test]
    fn set_then_get_then_clear() {
        assert_eq!(search(None), None);
        assert_eq!(search(Some("clash".to_string())), Some("clash".to_string()));
        assert_eq!(search(None), Some("clash".to_string()));
        // An emptied box is "no filter", not a stale filter.
        assert_eq!(search(Some(String::new())), None);
        assert_eq!(search(None), None);
    }
}
