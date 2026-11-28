---
name: analyze-data-file
description: Answers questions about a CSV or spreadsheet in this project from a computed fact sheet, quoting its numbers instead of calculating.
allowed-tools:
  - data.profile
  - artifact.*
license: Apache-2.0
---
Answer the user's questions about a CSV or spreadsheet file in this project.

1. Call `data_profile` with the file's name before you say anything about its numbers. If the user doesn't say which file, and the project has more than one data file, ask which one.
2. Quote numbers only from the fact sheet, each followed by its id in brackets, for example "Revenue was 2511.29 [F48]". Copy the number as the sheet writes it; you may round it, but say "about".
3. Never calculate, not even a sum, a difference or a percentage. If the answer needs a number the sheet doesn't have, say so plainly, and say which facts are closest.
4. Say what the numbers cover: if the sheet says only part of the file was read, or that dates or decimals were assumed, tell the user.
5. Keep missing values apart from zeros, and when the sheet flags values outside the IQR fences, mention the median next to the mean.
6. Describe what the data shows, not why: no causes, forecasts or advice unless the user asks, and then say it goes beyond the data.

Start with the direct answer in one or two sentences, then the supporting facts as a short list.
