# knxide-programmer

A web page for a KNX home: the lights, outlets and heating of the house, what controls each of
them and which output drives it. You change what a button does or how an output behaves, and
program the devices that are affected. It works from ETS project exports.

**Experimental.** It is built on `knxide-program`, which reproduces three recorded downloads
(masks 0021, 0701, 0705) request for request and has rules for nothing else. Everything a
recording does not show (a check that fails, lost frames, a full download taking the place of a
partial one, a download of edited values) follows behaviour notes and has not been seen on a
bus. Try `--simulate` first, and on a real installation start with *Compare with device*, which
only reads. See [On a real installation](#on-a-real-installation).

```sh
cargo run --release -p knxide-programmer -- --gateway 192.168.1.50
# then open http://127.0.0.1:8085 and bring a project file in

cargo run --release -p knxide-programmer -- house.knxproj --gateway 192.168.1.50
# the same, with house.knxproj listed and opened at start
```

## What the page does

The page itself explains nothing and shows no engineering facts; this file does.

- **Projects**: the page opens on the list of projects. *New project* brings in an ETS project
  file or makes a copy of a project of the list (with its edits). A project can be opened,
  renamed, copied, exported and removed. Inside a project, *Projects* leads back to the list.
  *Export* gives a `.knxproj`: the file that was brought in, with the edits written into it
  (see [Export](#export)).
- **Things**: the page does not start from devices. It works out the things of the house:
  runs of group addresses that belong together (switch, dim, status of one light) become one
  thing, the output that obeys and reports them is what drives it, the buttons whose group
  objects send to them are what controls it. See [How things are worked out](#how-things-are-worked-out).
- **Rooms**: one room at a time. On the left what is on the wall: push button sensors drawn
  with their rockers and LEDs where the product family is known (Gira push button sensor 3),
  any other device that sends as a plain list of its signals. On the right the things of the
  room. Wires join a rocker and its thing; hovering traces one. Dragging from a rocker to a
  thing, or choosing the thing in the panel, links every group object of the rocker to the
  address of the thing with the same size and direction, and the LEDs to its state.
- **Cabinet**: every device that is not a wall plate, by where it sits, with its outputs and
  the thing each one feeds; outputs that are in use by nothing are *Free*, those the
  parameters switch off are *Off*.
- **The panel** on the right, for what is selected, with a toggle that widens it:
  a *thing* (what controls it, what drives it, the groups it is part of, the LEDs that show
  it, the settings of its output, its addresses), a *rocker* (what it does in a sentence, the
  thing it controls, the product's settings for that rocker, its addresses), a free *output*,
  or a *device* (all parameter pages, the group objects with their addresses, *Compare with
  device* with what it found and *Take over what the device has*).
- **Loose ends**: what the chains of the project leave open, found without a bus: a button
  whose address no output obeys, an LED that waits for a state nothing reports, an output
  linked to another thing's address. Where one change mends it, the panel offers it.
- **Changes**: every device that differs from what it was last programmed with counts. The
  pill in the top bar opens one drawer that lists the changes per device in words, with
  *Discard* per device, and *Program N devices*. The devices are then programmed one after
  another; the drawer shows, per device, waiting, the phase under way with its progress, done,
  or failed with what the device has been told. A failure stops the run and leaves the rest
  pending; *Stop* stops the download under way and the run. A device with edits that cannot be
  programmed here is listed with the reason and is not counted. When routing runs over Wi-Fi
  the drawer says so above the button.

The view is in the address (`#projects`, `#rooms`, `#cabinet`). Light and dark follow the
system. Desktop and laptop widths. The typefaces are served by the server itself
(`static/fonts`, SIL Open Font License); the page asks no other host for anything.

Not on the page for now, though the server still answers for them: floor plans and the list of
group addresses (see the API).

## How things are worked out

This is the page's own reading of a project (`static/model.js`); nothing of it is stored. It
uses no words of any language except where said.

- **Things** are runs of group addresses in address order whose names begin alike: an address
  joins the run before it when it shares the run's words (`Hall ceiling switch`, `Hall ceiling
  dim`, `Hall ceiling status`), stays within nine addresses of it and is not a second command
  of the same size obeyed by another output.
- **Outputs** are the groups of group objects a product names alike (`Channel A`, `Output 1`,
  `Kanal 2`: a short list of words). An output is the thing whose state it reports and whose
  commands it obeys; a thing that several outputs obey, each being something else, is a
  group.
- **A thing counts** when an output drives it or a wall plate controls it. Values nobody acts
  on (weather, energy, alarms) are not things.
- **Rooms** come from the building structure. A thing is in the room its name begins with,
  else one its name mentions, else the middle group its addresses are in, else where the
  plates that control it are.
- **What a thing is called** (light, outlet, fan) is a guess from the kind of device that
  drives it and from words in its name in a few languages.

## Export

*Export* writes the project out as a `.knxproj`: the file that was brought in, entry for
entry, with `P-…/0.xml` changed as text where the edits say so (parameter values and group
object links per device, names given to group addresses, group addresses made here) and
`P-…/project.xml` where new identifiers were handed out. With no edits the original file is
returned byte for byte. The signature and certificate entries are copied as they are.

**Unverified against ETS.** The file reads back here as the edited project (a test does
that). Whether ETS imports a project whose content changed while its signature did not has not
been seen.

## Where edits are kept

The project file is never changed. Edits are kept in a file beside it, named after it:
`house.knxproj.knxide.json` (`--state FILE` puts it elsewhere for a project named at start).

The list of projects is kept in a directory (`--projects DIR`; by default `knxide/projects` in
the user's data directory): an index, `projects.json`, and one directory per project that was
brought in through the page, with the project file and its edits. A project named on the
command line stays where it is; it is only listed, and removing it from the list leaves its
files alone. It holds, per device, the
parameter values and group object links that differ from the project; for a device the app
has downloaded, when that was and the configuration that was sent (again the values and
links that differ from the project, with a digest of the download); and the group addresses
made or renamed here. It is written whole to a
temporary file and renamed, on every edit. A file that belongs to another project or cannot
be read stops the start; nothing is overwritten on a guess.

### What changed means

Every device has one baseline: the configuration the app last programmed into it, and the
project's where the app has never programmed it. Everything the page calls changed is
relative to that baseline, not to the project file. A value that was programmed as 16 and is
then set back to the project's 15 is one change, "16 -> 15", and is programmed like any
other; after a download that went through, nothing on the device is marked. Undo and
*Discard* go back to the baseline.

Three more things follow:

- A download that failed or was stopped after writing had begun leaves the device waiting
  until a download goes through, whatever the values are. The state file keeps that
  (`interrupted`), so it holds after a restart, and the drawer says so for the device.
- A comparison that read everything and found no difference means the device holds the
  configuration here. It becomes the baseline, whoever programmed the device.
- State files written before configurations were kept name only the digest of what was
  downloaded. Where the configuration here has that digest, it is the baseline. Where it
  has another, the device waits to be programmed and the drawer says that it differs from
  what was last programmed; which values differ is not known then, so none is marked, and
  undo and *Discard* have nothing to go back to. The next download that goes through
  writes the whole record.

Floor plans are kept the same way and never in the project: name and places in the state file
(places as hundredths of a percent of the image's width and height, so they hold at any size),
the images as files in the directory `<state file>.plans` beside it.

**ETS does not see what is changed here.** A device changed and downloaded here differs from
the ETS project until the same change is made there; a later download from ETS puts the
device back to what ETS has.

## Options

`knxide-programmer [PROJECT] [--projects DIR] [--gateway HOST[:PORT] [--tcp] | --routing [IP]
[--address A.L.D] | --simulate] [--state FILE] [--language LANG] [--listen IP:PORT]`

- `--projects DIR`: where the list of projects and the project files brought in through the
  page are kept.
- `PROJECT`: a `.knxproj` export, or a directory with its unpacked content, listed where it is
  and opened at start; without it the page starts with the list. It is only read,
  and only its XML files (`P-…/0.xml`, `P-…/project.xml`, `M-…/*.xml`); manufacturer baggage is
  never opened. A password-protected project is refused: export it without a password.
- `--gateway HOST[:PORT]`: tunnel through a KNX IP interface or router (port 3671). The tunnel
  is opened for each operation and closed after it. `--tcp` tunnels over TCP.
- `--routing [IP]`: KNXnet/IP routing. Frames are sent with TTL 1 from the individual address
  `--address` (default `0.0.2`), which nothing else on the installation may use. Without `IP`
  (or with `auto`) the interface is chosen each time the bus is used: a wired one that is up
  before a wireless one, then the one the system itself would use. A machine that gets its
  cable while the server runs is on the cable from the next operation on. With `IP`, the
  interface with that address is used whatever it is. Which interface it is, and whether it
  is wired, is printed at start ("routing on enp3s0 (wired) as 0.0.2") and in the log of each
  operation; the page shows only a warning when it is wireless or when there is no network.
  On Linux the
  interfaces come from `ip -4 -o addr show` and their kind from `/sys/class/net`; elsewhere
  the system's own choice is taken and its kind is unknown.
- `--simulate`: no bus; devices are simulated from the project, at about the pace of a real
  bus. For trying the page.
- Without any of these the page is for looking and editing only.
- `--state FILE`: where the edits of `PROJECT` are kept. Default: beside the project, see above.
- `--language LANG`: language of the products' texts where they have it, for example `de-DE`.
  Default `en-US`; a product without it shows its own default language.
- `--listen IP:PORT`: default `127.0.0.1:8085`.

**Security**: there is no login. Whoever can open the page can reprogram your devices. The
server listens on localhost by default and refuses requests that come from other websites'
pages. Use `--listen` with a LAN address only on a network you trust.

## On a real installation

Start with *Compare with device*. It only reads, it runs the same checks a download runs
first, and its result shows how good the connection is: how many frames had to be repeated,
how many answers asked for again, how many connections opened anew.

**Do downloads over a cable.** Routing is multicast, and Wi-Fi loses multicast: an access point
sends it once, without acknowledgement, often held back for sleeping stations. In one
measurement, with the same frames arriving at the same machine on both interfaces, about one
in ten was missing on Wi-Fi and none on the cable. A comparison copes with that: it reads again what it misses. A
download can recover from single failures (see below), but it never starts over, and one that
stops partway leaves the device without an application until a download goes through. A
machine with both a cable and Wi-Fi on the same network is on the cable unless `--routing` is
given the Wi-Fi address; when routing does run over Wi-Fi, the start says so, and the page
says so where devices are programmed.

## What a download does

1. Opens a transport connection, reads the mask version (it must be the application's), reads
   the maximum APDU length, presents the default access key.
2. Pre-check, all reads: serial number; for a product that prefers partial downloads, whether
   the device holds the application (load state, application identity); manufacturer;
   hardware identification; and, as this tool's own safeguard, the application identity where
   it has not been read yet. **A device of another manufacturer, with other hardware or holding
   another application is refused before anything is written**, and the page names the check.
3. The product's load procedure: load events, memory writes (each one echoed by the device or
   read back), restart.

It stops at the first answer that is not the expected one. A download that stops after step 2
leaves the device without a working application until a download succeeds, here or in ETS.
The individual address is never programmed.

One operation runs at a time. A frame that is not acknowledged within 3 s is repeated up to
3 times; an answer is awaited for 10 s, except where a shorter wait is named below.

A **download** never starts over, and it never sends a write a second time on a guess about
what the device made of the first. When an exchange fails, it finds out by reading, up to three
times per download:

- A read is asked again.
- For a write it reads what the write should have left: the memory or the property written, or
  the load state an event leads to (load: loading; load completed: loaded; unload: unloaded).
  If the device holds that, the download goes on as if the write's answer had arrived. If it
  does not and the device never acknowledged the write, the write is sent again. In every
  other case the download stops: when the device acknowledged the write and does not hold it,
  and for an event whose effect the load state cannot show (a segment event leaves "loading"
  as it is) that was never acknowledged.
- When the device acknowledged the request and only its answer went missing, the reading is
  done on the same connection: 1 s after an acknowledged repetition, or 4 s after a first
  acknowledgement, the answer is acknowledged unseen and the read follows, before the device
  gives the connection up. Otherwise a new connection is opened as at the start (connect,
  device descriptor, access key), and the device control setting of the download (echoing of
  memory writes) is read and, if the device dropped it, written again.

After the third recovery, or when a recovery fails, the download stops as it would have without
any. The job counts the recoveries and says how far it got: the steps completed, the step under
way and the last write the device took.

**This recovery is this tool's own.** It is not taken from behaviour notes on any other tool,
and it has only been run against simulated devices, never against a real one.

A **comparison** only reads, so it is persistent. Whatever goes wrong with the connection (no
acknowledgement, no answer, the device closing the connection), it opens a new one as at the
start and makes the failed read again, keeping what it has. It also gives a connection up as
soon as nothing has been heard from the device for 6 s, the time after which a device drops an
idle connection, instead of repeating into it. After 5 failures in a row on one read, or 60 new
connections in all, it gives up and returns a partial result: what was compared, what matched,
what differed and what could not be read. Every job counts the frames that had to be repeated
(by either side) and the new connections, so a bad link shows.

One kind of trouble is met without a new connection. On one device of mask 0021 a repeated
request was acknowledged and not answered again, and the device closed the connection six
seconds later. The reading of it: the device had answered the first transmission, and its
acknowledgement and its answer were both lost on the way. So when the repetition of a read is
acknowledged and no answer follows within 1 s, the comparison acknowledges the answer it never
saw and makes the read again as a new request; if that fails too, it opens a new connection as
for any other trouble. The job counts these as "answers re-requested". **This rests on an
inference of medium confidence** (two observations, from the receiving side only).

In its checking phase a comparison also reads the load states of the address table, the
association table and the application. A device that is not loaded is reported in words and
its memory is not read; a read that the device answers with no data is the device declining,
and no new connection is opened for it.

A comparison reads every byte the download would send. Where the device's group address table
has another number of entries than the plan's, parts of its tables lie where the download
sends nothing; those are read as well, so that the difference can be told in group addresses.
Its progress counts its own requests (the checking phase and the reads), not the download's.

## Limits of the editor

- What is shown, hidden and in use follows the product's own structure, evaluated by
  `knxide-program`. Products whose settings are made in a manufacturer's dialog (a plug-in)
  cannot be shown or edited; only their links are listed.
- A product that only asks for a manufacturer plug-in for its *download* has ordinary
  parameters, group objects and an ordinary load procedure. Such a device can be edited and
  compared. It is not downloaded: what the plug-in changes on the bus is not known, so the
  download stays disabled until a comparison or a recording has shown that the ordinary
  procedure gives the same result.
- Products made for older tools keep some editable parameters in blocks that are not shown;
  the page puts them on the page their display order points at. This is inferred from product
  data and unverified against ETS.
- Choices, whole numbers and texts are edited. Parameters of any other type (floating point,
  time, date, picture and so on) are shown with their value and cannot be changed here.
- No devices are added or removed, no product files imported, no individual address
  programmed. A project cannot be started empty: it comes from an ETS project file. Nothing
  is written back to the project file; an export is a new file.

## API

JSON over HTTP; the page uses nothing else. Requests that change something and carry an
`Origin` header that is not the server's own are refused with 403. Errors are
`{"error": "…"}`; 404 for an unknown device, 422 for a value or link that cannot be, 409 when
the state does not allow it.

The requests that work on a project answer 409 while none is open.

| Request | Answer |
|---|---|
| `GET /api/projects` | `{projects: [{id, name, file, owned, created_ms, opened_ms, summary}], open, library}`. `owned` is true for a project kept in the library; `summary` is what the page last worked out about the project (it puts it there itself); `open` is the identifier of the open project or `null`. |
| `POST /api/projects?name=…` | The body is a `.knxproj` file. It is read once and, when that works, kept in the library and listed: 201 with the project. 422 for a file that cannot be read as a project. |
| `POST /api/projects/{id}/open` | Reads the project with its edits and makes it the open one. 409 while the open project has an operation running. |
| `POST /api/projects/close` | No project is open afterwards. |
| `POST /api/projects/{id}/copy` | Body `{"name": "…"}`: a copy in the library, with the edits and floor plans: 201. |
| `PUT /api/projects/{id}` | Body `{"name": "…"}` and/or `{"summary": {…}}`. A name another project has gets a number. |
| `DELETE /api/projects/{id}` | Takes the project off the list; one kept in the library goes with its files. |
| `GET /api/projects/{id}/export` | The project as a `.knxproj` with the edits in it, as a download. See [Export](#export). |
| `GET /api/model` | What the page builds rooms and things from: `{project, bus, spaces: [{path, kind}], devices: [row + objects, unused, pages: [{id, title, channel}]], addresses, ranges, job, queue}`; a row and an object as below, addresses and ranges as in `GET /api/group-addresses`. |
| `GET /api/project` | `{version, project: {id, name, source}, state_file, bus: {mode, text}, supported, devices: [row…], job, queue}`. A row: `id`, `address`, `sort` (the address as a number), `name`, `location: [..]` (building part to room), `order_number`, `product`, `application: {id, name, number, version, mask}`, `supported`, `kind` (`button`, `dimmer`, `switch`, `heat`, `blind`, `sensor`, `net`, `box`) and `type` (the same in words), `attention` (what the last operation found, or `null`), `editable`, `comparable`, `download_blocked`, `reason` (one plain sentence), `obstacles: [{code, text, plain}]`, `state` (`edited` while the device waits to be programmed, `downloaded` when the app knows it holds what is here, else `project`), `edits` (the number of parameters and group objects that differ from the baseline), `pending_note` (one sentence for a device that waits without a change to name, else `null`), `downloaded_ms`, `last` (the last finished job on the device). `supported` means the device can be downloaded; `editable` and `comparable` are also true for a device whose product only asks for a download plug-in, which then has the reason in `download_blocked`. The overview's `supported` and `editable` count them. `bus.mode` is `none`, `tunnel`, `routing` or `simulated`; for routing, `bus.interface: {name, address, kind}` (`wired`, `wireless`, `unknown`), `bus.wireless` and, over Wi-Fi, `bus.warning`. |
| `GET /api/devices/{id}` | The editor payload: the row plus `talks_to` (the devices that share a group address with it), `pages`, `objects`, `unused`, `changes`, `from_project`, `download: {kind, falls_back_to_full, phases}` or `download_error`, `capacity: {addresses, associations}`, and `findings` of the last comparison while they still apply. See below. |
| `PUT /api/devices/{id}/parameters/{reference}` | Body `{"value": "…"}`. Sets a parameter and answers with the new editor payload plus `saved` and `save_error`. A value equal to the baseline's is no change; one equal to the project's is, when the device was programmed with another. |
| `DELETE /api/devices/{id}/parameters/{reference}` | Back to the baseline's value: what the device was last programmed with, or the project's. Answers like `PUT`. |
| `PUT /api/devices/{id}/objects/{reference}` | Body `{"links": ["1/1/1", …]}`: the object's group addresses in order, the first one sending. Answers like `PUT` of a parameter. |
| `DELETE /api/devices/{id}/edits` | Discards every change of the device: back to the baseline. |
| `POST /api/devices/{id}/adopt` | Body `{"references": [..]}` or `{}` for all: takes the device's values from the last comparison over as edits. 409 without a comparison that still applies. |
| `GET /api/devices/{id}/technical` | `mask`, `manufacturer`, `hardware`, `parameters_in_use`, `objects_in_use`, `tables` (`addresses`, `associations`, `objects`, and where each table sits), `segments: [{address, data}]` (hex), and `variants`: the download as prepared and, for a partial one, the full download it turns into. A variant: `kind`, `bytes_sent`, `sent` (per segment a string of `0`/`1`, one per byte), `writes`, `phases`, `requests: [{text, class}]`, `error`. 422 when there is no plan. |
| `GET /api/devices/{id}/plan` | `{id, address, supported, variants}`: the plan alone. No bus is touched: the requests are those against a modelled device that holds the application and has empty memory. |
| `POST /api/devices/{id}/compare` | Starts a comparison: 202 `{job}`. 422 device not comparable, 409 no bus configured or another operation running. |
| `POST /api/devices/{id}/download` | Body `{"confirm": true}`. Starts a download of the device as edited: 202 `{job}`. 400 without the confirmation; 422 for a device that is not supported, with the reason, also when it is comparable; otherwise as above. |
| `GET /api/group-addresses` | `{addresses: [{address, raw, name, datapoint_type, source, project_name, used_by: [{device, device_name, device_address, number, object, function, sending, in_use}]}], ranges: [{start, end, name, level}]}`. `source` is `project`, `renamed`, `created` or `unknown` (linked, but known to neither). |
| `POST /api/group-addresses` | Body `{"address": "1/2/3", "name": "…", "datapoint_type": "DPST-1-1"}` (the type is optional): 201. 409 when the address exists. |
| `PUT /api/group-addresses/{raw}` | Body `{"name": "…"}`. An empty name gives a project's address its own name back. |
| `POST /api/program` | Body `{"devices": [id, ..], "confirm": true}`. Programs the devices one after another, in that order, each as an ordinary download job: 202 `{queue}`. Nothing starts unless every device can be programmed: 404 for an unknown one, 422 for one that cannot be downloaded (named, with the reason) or is named twice, 400 without the confirmation or without devices, 409 without a bus or while something runs. A download that does not succeed stops the queue; so does `POST /api/job/cancel`, which also stops the download under way. |
| `GET /api/program` | `{queue}`: the queue at work or the last one, or `null`. A queue: `id`, `active`, `entries: [{device, name, status, job, error}]` with `status` one of `waiting`, `running`, `done`, `failed`, `pending` (the queue stopped before its turn). |
| `GET /api/floor-plans` | `{plans: [{id, name, image, positions: {<device id>: {x, y}}}]}`. `image` is the address of the image; `x` and `y` are percent of its width and height. |
| `POST /api/floor-plans?name=…` | The body is the image: PNG, JPEG or WebP, told by its first bytes, at most 8 MB. 201 with the plan. 415 for anything else (SVG included), 413 when too large. |
| `PUT /api/floor-plans/{id}` | Body `{"name": "…"}` and/or `{"positions": {<device id>: {"x": 0–100, "y": 0–100}}}`; the positions given replace all of the plan's. 422 for a place out of range or a device that is not in the project. |
| `PUT /api/floor-plans/{id}/image` | Another image for the plan; the places stay. |
| `GET /api/floor-plans/{id}/image` | The image. |
| `DELETE /api/floor-plans/{id}` | Removes the plan and its image. |
| `GET /api/jobs` | `{jobs: [..]}`, newest first. `GET /api/jobs/{id}`: `{job, log}`. |
| `GET /api/job` | `{job, log}`: the running or last job and its log. |
| `POST /api/job/cancel` | `{cancelled: bool}`. The running operation stops at its next request and the connection is closed. |
| `GET /ws` | WebSocket. First `{type: "hello", job, log}`, then `{type: "job", job}` when a job starts or ends, `{type: "progress", job, sent}` per request (with `expected`, `repetitions`, `re_requests`, `reconnects`, `recoveries` or `phase` when they change), `{type: "queue", queue}` when a programming queue moves, `{type: "log", line}`, `{type: "edited", device}`. `hello` carries the `queue` as well. |

The editor payload:

- `pages: [{id, channel, title, items}]`. An item is `{type: "text", text}` or a parameter:
  `reference`, `label`, `control`, `value`, `value_text`, `baseline_value`, `baseline_text`,
  `default_text`, `standing` (`default`, `project`, or `edited` for a value other than the
  baseline's), `suffix`. A control is
  `{type: "choice", choices: [{value, text}]}`, `{type: "number", minimum, maximum}`,
  `{type: "text", max_length}` or `{type: "fixed"}` (shown, not edited).
- `objects` (in use) and `unused` (linked, not in use): `reference`, `number`, `name`,
  `function`, `size_bits`, `datapoint_type`, `priority`, `flags: {communication, read, write,
  transmit, update, read_on_init}`, `links: [{address, raw, name, datapoint_type}]`.
- `changes`, relative to the baseline: `parameters: [{reference, page, label, from, to}]`
  (`page` is the title of the page the parameter is on), `links: [{reference, number, name,
  added, removed, now, sending_changed}]`, and what the parameter changes do to the group
  objects: `dropped` (linked objects that went out of use) and `gained` (objects that came
  into use), as objects.
- `from_project`: what differs from the project file, as `parameters` and `links` like in
  `changes`, with `from` the project's value.
- `findings` (also in the result of a comparison): `{type: "parameter", reference, page,
  label, here, device, adoptable}`, `{type: "links", reference, number, name, here, device,
  adoptable}`, `{type: "bytes", address, here, device, what}`.

A job: `id`, `kind` (`compare`, `download`), `device`, `device_name`, `address`, `status`
(`running`, `succeeded`, `incomplete`, `failed`, `cancelled`), `started_ms`, `finished_ms`,
`sent`, `expected` (requests the job expects to send in all: a download's plan, or a
comparison's own checking phase and reads, corrected as it goes; equal to `sent` when the job
has succeeded), `phases`, `phase`, `repetitions`, `re_requests` (comparisons: answers asked
for again on the same connection), `reconnects`, `recoveries` (downloads: failed exchanges
recovered from by reading), `error`, `check` (the check
that stopped it: `mask`, `access`, `manufacturer`, `hardware`, `application`, `partial`),
`written` (for a job that stopped: what the device has been told, in a sentence), `progress`
(for a download that stopped: `steps`, `steps_completed`, `failed_step`, `writes`,
`last_write`) and `result`.

The result of a download: `kind`, `full_because`, `requests`, `repetitions`, `recoveries`,
`serial_number`, `notes`. Of a comparison: `kind`, `full_because`, `serial_number`,
`application` (what the device reports), `notes`, `complete`, `stopped_because`, `refused`
(it stopped because the device answered a read with no data), `not_loaded` (for a device that
is unloaded or partly loaded: the sentence; its memory was not read), `load_states:
{address_table, association_table, application}`, `planned`, `compared`, `matching`,
`differing`, `unread_bytes`, `unread: [{address, length}]`, `repetitions`, `re_requests`,
`reconnects`, `requests`, `findings`, `differences: [{address, planned, device}]` (hex). A comparison that
had to give up ends as `incomplete` with `complete: false`; its figures cover what was read.

A log line: `seq`, `time_ms`, `job`, `level` (`request`, `response`, `note`, `info`, `error`),
`text`. Requests and answers are in the one-line form of `knxide-program`
(`MEM_WRITE 4000 0102`, `PROP_RESPONSE obj=0 pid=12 start=1 count=1 00FA`).

## Trying it without a bus

```sh
cargo run -p knxide-testkit --example demo_project -- demo.knxproj
cargo run -p knxide-programmer -- demo.knxproj --simulate
```

The demo project is made up: three devices that can be edited and downloaded (one per
supported mask), one that can be edited and compared (its product asks for a download
plug-in), and four that cannot be used, each for another reason. `--simulate` works with a real
project as well; the simulated devices then hold the project's applications and empty memory.
