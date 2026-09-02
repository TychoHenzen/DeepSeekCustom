/// Exact harness-owned input recognized before ordinary chat dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlledDevelopmentControlInput {
    Status,
    Map,
    Diff,
    Why(String),
    Stop,
}

impl ControlledDevelopmentControlInput {
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "STATUS" => Some(Self::Status),
            "MAP" => Some(Self::Map),
            "DIFF" => Some(Self::Diff),
            "STOP" => Some(Self::Stop),
            _ => input.strip_prefix("WHY ").and_then(|item| {
                (!item.is_empty() && item.trim() == item).then(|| Self::Why(item.to_string()))
            }),
        }
    }
}
