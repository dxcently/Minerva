# src/notes/ — the Notepad (`/hub/notes`)

Notes are `<home>/notes/<name>.md`, `0600`, in a `0700` folder, with a trash beside them at `<home>/trash/notes/`. Sending one as mail
is `mail`'s (`POST /hub/notes/<n>/send` is a notes route that hands the request to `mail::send_note` once the name has passed).

```text
mod.rs    State (the lock), route(ctx, req, rest) -> Response<BoxBody>, create(ctx, title, text) -> Created, name_ok, title_of   (N1)
```

```text
GET  /hub/notes                      [{name, title, size, mtime}], newest first, 2000 at most
POST /hub/notes {text?, title?}      201 {name, sha, mtime}; a birth name (`names::birth_name`); the text is `title`, a blank line, `text`
GET  /hub/notes/<n>                  {name, text, sha, mtime}
POST /hub/notes/<n> {text, base}     200 {sha, mtime} | 409 {error, sha} | 413 | 400 (NUL)
POST /hub/notes/<n>/rename {title}   200 {name}; the file becomes `names::slug(title)`, `-2`, `-3` on a clash
POST /hub/notes/<n>/trash            204
POST /hub/notes/<n>/send {..}        mail::send_note
GET  /hub/notes/trash                [{name, title, size, mtime}]
POST /hub/notes/trash/<n>/restore    200 {name} | 404 | 409 (a note has that name)
POST /hub/notes/trash/empty {confirm: true}    200 {removed}
```

| Rule | What it is |
|---|---|
| name | `^[a-z0-9][a-z0-9-]{0,47}$`, and not `trash` (a route). A request's path is split on `/` and never percent-decoded; a name becomes a `Name` before it is joined to a path. A bad name is the empty 404 |
| etag | the sha256 of the file in lower-case hex. A save names its `base`; a base that is not the file's sha now is a 409 carrying the sha now. The same text again writes nothing |
| size | 256 KiB of UTF-8 per note (413), no NUL (400). A JSON body is read under 6 x that, the worst case of every byte escaped |
| count | 2000 listed. A create or restore that would make the 2001st is a 409, and so is a trash that holds 2000 |
| title | the first non-empty line, control characters dropped, 120 characters at most. A listing reads 4 KiB of each note for it |
| files | written by `write_atomic_0600`; read through `rawfile::open_regular` (`O_NOFOLLOW`, `O_NONBLOCK`, then `fstat` must say a regular file). A link, a FIFO, a directory, a dotfile or a temp file is not a note |
| folders | a plain directory of the perch's user, made `0700`, a looser one tightened; a link to a directory is a 503 and nothing is written through it |
| trash | a rename into `<home>/trash/notes/`, so the text is whole; a name already waiting there gets `-2`. The chats' trash reads only `*.eid` directly in `<home>/trash`, so the two do not meet |
| empty | removes the listed notes of the trash (regular `<name>.md`) and nothing else: not a link, a folder, a stray file or a chat log |
| bodies | JSON objects only (`["a", "b"]` is not `{"text": "a", "base": "b"}`); read through `serve::body_of` |
| lock | `State` holds one mutex; every call that changes something does its file work on a blocking thread under it, so a check and the write or move after it do not interleave inside this perch. Reads (list, GET, `load`) take no lock: a note is replaced whole by a rename |

**For mail.** `create(ctx, title, text)` is what "save as note" calls: it makes the note exactly as `POST /hub/notes` would and gives back
`Err(Box<Response>)` (the refusal to send) or `Created {name, sha, mtime}`. `name_ok(&str)` says whether a string is a note's name, and
`MAX_TEXT` is the 256 KiB. `load(ctx, name) -> Result<Loaded {name, title, text, sha}, Box<Response>>` is the safe re-read for
`POST /hub/notes/<n>/send`: the name checked, the file opened without following a link, UTF-8 and 256 KiB enforced, no lock taken.

A perch started without `--home` has no notes: every `/hub/notes` route (and `create`, `load`) is the empty 404, as it was before this slice.
Notes are only ever one folder deep and never leave the home. Nothing here logs a note's text or a title; a failed file call logs the
`io::Error` and the page is told "that could not be done in the notes folder" (503).
