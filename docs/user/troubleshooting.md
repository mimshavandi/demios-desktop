# Troubleshooting

## "Connect a model to start chatting"

No model is ready yet. Select **Connect a model** and add one: download a model on this device, or save a provider key. See [Adding a model](models.md).

## A model shows "Won't fit" or "Memory pressure"

Your computer probably doesn't have enough memory for that model. Choose a smaller one from the list; those near the top of the catalogue are the smallest. Closing other apps can help a model marked **Caution** or **Memory pressure**.

## A download failed or stopped

Select **Retry** (or **Resume** if you paused it). The download continues from where it stopped. If it keeps failing, check your connection and that there's enough free disk space for the size shown on the row.

## An answer failed

The error under the answer says what happened. Select **Retry** to try again.

- **The provider stopped responding.** If a provider sends nothing for 5 minutes, or goes silent for 60 seconds in the middle of an answer, the answer stops with an error. Retry, or check the provider's status page.
- **A key problem.** Check that the key is still valid in your account with the provider, then paste it again in **Settings → Models → Provider keys**.

## macOS asks for keychain access

Provider keys are stored in your system keychain. macOS may ask whether Demios can use it. Allow it, or the app can't read your saved keys.

## A tool call is stuck on "Did this go through?"

See ["Did this go through?"](tools-and-approvals.md#did-this-go-through).

## Remove one downloaded model

There's no button for this in the app yet. Downloaded models are in the folder shown in **Settings → General → About**; you can delete a model's files there with the app closed. To remove all of them along with everything else, use **Delete all local data** (see [Privacy and your data](privacy.md)).

## Still stuck

[Report a problem](reporting-a-problem.md).
