# webui/term/install

The install block on the start screen and the update keys overlay (W7): what `GET /hub/install` says about this Minerva, and how an
update key is added to or removed from the list the perch shows. The perch does the trusting (`perch/src/install/`); this folder only
shows and asks. Nothing here updates anything: the page has no update action, and the notice names `hoot update` as text for a person to
run in a terminal.

| file | does |
|---|---|
| `keys.js` | the update keys overlay (`KeysPanel`, opened through core/overlay.js by `[ update keys ]`): the list (`GET /hub/install/keys`), `[ inspect ]` (`POST …/keys/inspect`), the fingerprint typed back, `[ add key ]` (`POST …/keys/add`), `[ remove ]` on the rows added by hand (`POST …/keys/remove`). Its pure helpers (`normFp`, `sameFp`, `addReady`, `canRemove`, `removeReady`, `writeOutcome`, `unsureNote`, `keysView`, `stepUp`) are what tests/keys.test.mjs holds |

`panes/home.js` holds the install block itself (`InstallBlock`, `installLines`, `keysOffered`, `jevWords`): the notice, the plugins line,
the jev line, and `[ update keys ]` for an installed Minerva only.

## The flow

```text
 start screen                       overlay (keys.js)                      perch (installed minerva)
 GET /hub/install  --- lane --->    (opened by [ update keys ])
                                    GET  /hub/install/keys        ----->   keys list --json
                                    paste .pub, [ inspect ]       ----->   POST …/keys/inspect   -> id, fingerprint, comment
                                    type the fingerprint back (any case, any spacing)
                                    [ add key ] (only when it matches) -->   POST …/keys/add {pubkey, confirm}
                                                                              403 unless a person ran, in a terminal:
                                                                                hoot keys allow-web <FP>, then add within 2 minutes
                                    re-list after every answer that may have changed the list
```

## Rules the page holds

| Rule | Where |
|---|---|
| A fingerprint is 16 hex digits; case and spacing are ignored; anything else matches nothing | `normFp`, `sameFp`, `addReady` |
| `[ add key ]` and `[ remove key ]` are enabled only when the typed fingerprint is the key's, and no write is running | `addReady`, `removeReady` |
| `[ remove ]` is offered only for a key whose source is `added by hand` (ruling 6) | `canRemove` |
| A 403 is shown as the perch sent it (the terminal command); the page never arms | `writeOutcome`, `install/keys.js` has no arm route |
| A 5xx, or no answer, is read by its status alone: the change may have been made, so the list is read again and the page says what it shows | `writeOutcome`, `unsureNote` |
| Every answer's text is shown as text: a key's comment in its own element (`dir=auto`, `unicode-bidi: isolate`, clipped, fixed line height), apart from the fingerprint | `KeysPanel` |
| Person actions (inspect, add, remove, the list when the panel opens) are `hubGet`/`hubPost`; the notice is a poll through the request lane | `install/keys.js`, `panes/home.js` |
| The panel takes its own keys: ctrl+enter, `:`, `?` and Tab typed in it never reach the page's keys (the new-pane menu and the rest) | `keyOwners.add` in `install/keys.js` (the `.pane.keys` section) |

## What the page does not defend

A script running in this page's origin could show another fingerprint than the key's, so the person is told to compare it with what the
terminal printed and with the maintainer's published one, not only with this page. The terminal arm guards against this page alone; it
does not guard against someone who holds this page's cookie once web terminals exist. Agents run as the person: an agent can add a key
through the CLI without this page, and Minerva's guard rows only slow that down. Each row of the list says whether its key expires; the
page does not say more about trust than the list shows.
