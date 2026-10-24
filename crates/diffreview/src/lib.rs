//! The model of a diff review, after [revdiff](https://github.com/umputun/revdiff):
//! a diff parsed into files and lines, annotations on lines, hunks and whole
//! files, and those annotations written out in revdiff's own format, so that
//! whatever consumes revdiff's output (its agent plugins, scripts) consumes
//! this too.
//!
//! No interface and no async: [`git::LocalRepo`] runs git in a directory,
//! which is all a server needs. An application that has the repository
//! elsewhere (Aiddiction: on a server) asks for the same
//! [`source::DiffRequest`] its own way and hands the text to [`diff::parse`].
//! The gpui-kit view over this is the `diffreview-ui` crate.

pub mod annotation;
pub mod diff;
pub mod git;
pub mod prompt;
pub mod source;
pub mod tree;

pub use annotation::{Annotation, ChangeType, Store};
pub use diff::{DiffLine, FileDiff, FileStatus, LineKind};
pub use source::{DiffRequest, DiffSource, Refs};
