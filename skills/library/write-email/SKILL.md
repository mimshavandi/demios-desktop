---
name: write-email
description: Drafts a new email or a reply to one the user pastes, ready to send as it is.
allowed-tools: []
license: Apache-2.0
---
Help the user write an email they can send without editing.

**What to write**
- If they paste an email to answer, read it first. Reply to every question and request in it, in the order it raised them.
- If they describe what they want to say, write a new email from that.
- Lead with the point: the request, the answer or the news goes in the first two sentences. One idea per paragraph. End with the next step when there is one: what you need, from whom, by when.
- Match the tone to the situation, unless they ask for a tone: friendly with colleagues, more formal with people they don't know, calm when something went wrong.
- For a new email, start with a line "Subject: …" that says what the email is about. Skip it for a reply.

**Only real details**
- Never invent facts: no names, dates, times, amounts, prices, order numbers, places or promises that the user or their material didn't give you.
- Never use bracket placeholders such as [date], [Your name] or [Company]. If a detail the email needs is missing, write the email without it, then list what's missing in one line after the draft, for example "Missing: the new delivery date".
- If something is attached or pasted (an invoice, a quote, a report), use its actual details: the amount, the date, the reference number. Don't write "as shown in the attachment" when you can say what it shows. Only mention an attachment the user actually has.
- Recipients: use only names and addresses the user gave you or that appear in the email they're answering. Never make up an email address. If you don't know who it goes to, write the email anyway and ask who it's for.
- Sign with the user's name only if you know it. Otherwise end without a name.

**Output**
Write only the email. After it, add the "Missing" line if you need one, then one short line offering a shorter or more formal version.
