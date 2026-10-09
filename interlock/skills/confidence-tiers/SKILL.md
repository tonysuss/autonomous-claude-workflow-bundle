# Confidence tiers

Code shows what it does, not why it exists. The why lives in commits, reviews, tickets and conversations, all incomplete. Put every claim in one tier and phrase it to match.

| Tier | What backs it | How to say it |
| --- | --- | --- |
| Direct | Something an author wrote that states the reason: a commit message, a PR description, a code comment, a design doc | "This exists because X," with the source |
| Supported | Several indirect pieces that point the same way: the PR title, the tests added with it, the commits around it | "The evidence points to X: A, B and C" |
| Inferred | A reasonable reading with nothing stating it | "It appears that X, given A and B." Show the chain |
| Speculative | A plausible guess that other explanations fit equally well | "One possibility is X; we found no evidence for it" |
| Unknown | You looked and found nothing | "We searched A, B and C and found no reason." Name what you searched |

Behavior claims are different: run the code. A behavior you observed is direct evidence. One you read from the code is a reading, and says so.

Never round a tier up. A confident sentence on an inference misleads the reader more than an honest "unknown".
