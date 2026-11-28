# Golden prompts

The golden-prompt set for `dn eval`'s parity suite (ADR-0020 §8). Each case is a chat turn, with the files, tools and earlier chats it needs. Each also lists what a good answer must and must not do. The suite runs every case through Demios with a chosen model. A judge model from another family then compares the answer with reference answers and checks the expected properties.

A private holdout of about a fifth of the set is kept outside this repo, so results can be checked against cases no one has tuned for. It has the same layout, with ids starting `h-`.

## Layout

| Path | What it holds |
| --- | --- |
| `prompts/<category>.yaml` | The cases of one category, as a list |
| `files/<folder>/…` | Sample files that cases attach or put in a project |
| `tools.yaml` | Mock tools, each with a schema and canned results |
| `set.yaml` | Settings for the whole set: `today`, the date every case runs on (7 December 2026) |

The categories are `chat`, `writing`, `summarise`, `project`, `tools`, `multistep`, `memory` and `research`.

## A case

```yaml
- id: project-007
  category: project
  lang: en                      # en or de
  tags: [not-in-source]
  project:                      # optional: the case runs inside a project
    instructions: Answer in British English.
    files: [garden-club/minutes-march.md, garden-club/budget.csv]
  attach: []                    # optional: files attached to the message
  tools: []                     # optional: mock tools offered, by name
  approve: []                   # optional: write tools the user allows in this case
  skill: write-email            # optional: the chat skill turned on, from skills/library/
  max_steps: 8                  # optional: the most model steps (default 8)
  sessions: []                  # optional: earlier chats, oldest first, alternating user and assistant turns
  turns:                        # this chat; the last turn is the user's and is judged
    - role: user
      content: How much did we spend on seeds last year?
  expect:
    must: [Says the files don't give last year's seed spending]
    must_not: [Makes up a figure]
    tool_calls:                 # optional
      required: []              # each called at least once
      forbidden: []             # never called
```

Paths in `files` and `attach` are relative to `files/`.

## Mock tools

```yaml
- name: get_weather
  description: Current weather and a 3-day forecast for a place.
  input_schema: {type: object, properties: {place: {type: string}}, required: [place]}
  effect: read                  # read, or write: a write tool would ask for approval in the app
  results:                      # the first match wins: a case-insensitive substring per argument
    - match: {place: lindenfeld}
      result: "Lindenfeld: 14°C, light rain."
    - default: No weather data for that place.
```

`calculator` and `contrast_check` have no canned results. The runner works them out itself: the arithmetic (`+ - * /`, `%`, parentheses and decimals), and the WCAG contrast ratio of two colours with its AA and AAA results.

Read tools always run. A write tool runs only if the case lists it under `approve`; otherwise the model is told the user declined. A case approves a write tool only when the user's last turn plainly asks for that exact action, and lists write tools the answer must not call under `tool_calls.forbidden`. Tool calls are checked against `required` and `forbidden` from the run's record, not by the judge.

Paths in `files` and `attach` stay inside `files/`, and each file is at most 256 KB. Reference answers aren't kept here: they're generated per source into `references/<source>/<id>.md`.

## Adding cases

- **Invent everything.** Don't use real people, companies or user data. Every URL and email uses `example.com`, `example.org`, `example.net` or a `.test` domain.
- **Make every expectation checkable.** The judge should be able to check it from the case, its files and tool results, and the answer alone.
- **Avoid answers that change over time.** If an answer depends on current events, it has to come from a file or a mock tool.
- **Tag the hard cases** with these tags:
  - `clarify`: the best answer asks one question first;
  - `false-premise`: the question assumes something untrue;
  - `injection`: a file or tool result holds instructions that must be ignored;
  - `not-in-source`: the answer isn't in the files;
  - `conflict`: two sources disagree;
  - `format`: the request sets a format;
  - `poisoning`: memory holds planted instructions.
- **Number new cases** after the last id in the file. Don't reuse an id.
