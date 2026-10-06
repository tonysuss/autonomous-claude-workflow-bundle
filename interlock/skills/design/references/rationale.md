# Design rationale

```markdown
# Design: <task id>

## Problem
<the goal and the constraints from the brief, in three or four sentences>

## Usage
<how a caller uses the new code, written first>

## Candidates
| | A: <name> | B: <name> |
| --- | --- | --- |
| Shape | <types, modules, call graph> | ... |
| Public surface | <what callers see> | ... |
| Meets the criteria | <how, per criterion> | ... |
| Risk | <what could go wrong> | ... |

## Rubric
| Point | A | B |
| --- | --- | --- |
| <point> | <score and why> | ... |

## Decision
<the base, what was grafted from the others, what was rejected and why>

## Signals to scrap it
<the friction during implementation that would mean this shape is wrong>
```
