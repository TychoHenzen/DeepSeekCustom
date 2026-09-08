/// The exact exception and changed dependency that authorized one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyExceptionMatch {
    pub path: String,
    pub dependency_name: String,
    pub complexity_exception: String,
}
