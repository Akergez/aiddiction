//! What an agent is told when a review is handed to it: the loop revdiff's
//! agent plugins run, said once in the message instead of in a plugin.

/// The message carrying `output` (revdiff's format) to an agent.
pub fn for_agent(output: &str) -> String {
    format!(
        "Here is my review of the current changes, as annotations in revdiff's format: \
`## file:line (+)` is an added line, `(-)` a removed one (old line number), `( )` an unchanged one, \
`file:start-end` a whole hunk, `(file-level)` the whole file.\n\
\n\
Annotations that ask a question (they contain `??`, or start with explain, describe, what, how, why, clarify) \
want an answer, not a change: answer them first. Address every other annotation by changing the code. \
When you are done, say briefly what you changed for each.\n\
\n\
{output}"
    )
}
