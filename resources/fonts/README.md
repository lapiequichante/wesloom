# Default fonts

The renderer embeds no font (ADR 0014) — an application supplies the
bytes. These two are *this repository's* defaults: the editor example and
the screenshot tests load them first, so the interface renders the same
text on every machine and the tests never skip for a missing system font.
`--font`/`--mono` on the editor override them.

| File | Face | License |
|---|---|---|
| `Inter-Regular.ttf` | the interface (variable font; the default instance is what renders) | SIL OFL 1.1 — `LICENSE-Inter.txt` |
| `JetBrainsMono-Regular.ttf` | the code panels | SIL OFL 1.1 — `LICENSE-JetBrainsMono.txt` |

Both are unmodified upstream files, redistributed under the SIL Open Font
License, Version 1.1; the license texts beside them travel with any
redistribution (their terms, not ours). Replacing either face means
replacing its license file too.
