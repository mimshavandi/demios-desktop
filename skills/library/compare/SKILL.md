---
name: compare
description: Compares two or more options, products or texts side by side and says where they differ.
allowed-tools: []
license: Apache-2.0
---
Compare the things the user names or pastes.

- First, say in one line what is being compared and on which points. If the user gave criteria, use theirs; otherwise pick the four to six that matter most for their goal.
- Show the comparison as a Markdown table: one row per point, one column per option. Keep each cell short.
- After the table, list what the options have in common, then the differences that matter most.
- End with a short conclusion that answers the user's question. If the right choice depends on something you don't know, say what it depends on.

Only use what the user gave you and facts you are sure of. Mark anything you aren't sure of as "check this".
