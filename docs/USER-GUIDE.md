# Remote Control: user guide

Remote Control lets you see another computer's screen, and, if its owner
allows it, use its mouse and keyboard, copy text and send files. The person
at the shared computer decides every time. Nothing starts by itself, nothing
runs in the background, and closing the window ends sharing.

## Install

Run `Remote Control_<version>_x64-setup.exe`. It installs **for your user
only**, needs no administrator rights, and puts a shortcut in the Start menu.
It does not set the app to start with Windows, and does not install a
service. If Windows' WebView2 component is missing (it is part of current
Windows 10 and 11) the installer downloads it from Microsoft.

**This installer is not signed.** Windows may show "Windows protected your
PC" (SmartScreen) and, if Smart App Control is on, may refuse to run it at
all. Check the file against the SHA-256 checksum published with it
(`Get-FileHash <file>`), then choose "More info" → "Run anyway". Turning
Smart App Control off is a system setting that is yours to decide; this
project does not ask you to. See `docs/RELEASING.md` for why it is unsigned.

To uninstall: Settings → Apps → Remote Control, or run the uninstaller in the
install folder. Your settings and keys stay (see "Where things are") so that
reinstalling keeps the same device identity; delete the folders to remove
them.

## The two halves

The window has **Share this screen** (this computer is the one being viewed)
and **View a screen** (this computer looks at another). You can use either,
or both.

### Share this screen

1. Pick the **Display** (one display is shared per session; the choice is
   fixed once sharing starts).
2. Set what a viewer may do besides watch. All three start **off**:
   - *Let the viewer control this computer's mouse and keyboard*
   - *Let the viewer sync clipboard text with this computer*
   - *Let files be sent between the computers* (each file still needs
     your acceptance)

   You can change these at any time while sharing; the viewer is told at
   once and loses the ability at once.
3. Press **Start sharing**. Nothing is captured yet.
4. When someone connects, a prompt shows their **name**, their **key** and a
   **six-digit code**. The viewer's screen shows the same code. **Compare
   them.** If they differ, press **Deny**: someone may be in between. The
   name is only what that device calls itself; the key is what identifies it.
5. **Allow** starts the picture. **Deny**, or no answer within the timeout
   (30 s by default), refuses. A device you allowed and ticked "Remember" is
   **still asked every time**: remembering only tells you that you have seen
   its key before.
6. **Stop sharing**, or closing the window, ends the session immediately.

Two things to know: the lock screen and Windows' secure prompts (such as an
administrator confirmation) cannot be captured, so the viewer sees a pause
and the window says "Waiting: the lock screen or a system prompt is
showing"; and only one viewer at a time is served, the next is told the host
is busy.

By default **only this computer** can connect. To let others, turn on *Let
other computers connect while I am sharing* in Settings. Then other
computers on your network can reach you at the address shown, and (with *Let
devices on the local network find this one while sharing*) show up in their
"Find devices" list. Windows may ask whether to allow the app through its
firewall the first time; that depends on your network profile and was not
observed during development.

### View a screen

Enter **how to reach the other computer** and press **Connect**:

- an **address** such as `192.168.1.20:51234` (the sharing computer shows
  it), or a bare port number for this computer;
- or a **device ID**, the 16 digits (`C79F-D0DD-92D3-EA9A`) the sharing
  computer shows as its "Device key". This needs a server in Settings (see
  `docs/DEPLOYMENT.md`) and works across networks;
- or press **Find devices on this network** and pick one. The list shows
  each device's name (what it says about itself) and its **key**; the
  connection is refused if the device that answers has a different key than
  the one listed.

The window shows who answered and the code to compare with the person at the
other end. Until they allow it you see "Waiting for the person at the other
device". Then:

- *Control the shared screen*: your mouse and keyboard go to the other
  computer, if its owner allowed control. If not, the box stays unavailable.
- *Sync clipboard text with the host*: needs the host to allow clipboard
  **and** you to tick it. Turning it on never sends what is already on your
  clipboard. Text marked private by the program that copied it (password
  managers do this) is never sent. Text only; no images or files.
- *Send a file to the host / to the viewer*: the receiving person is asked
  for each file, with its name and size. Files are saved in
  `Downloads\Remote Control`, never overwrite another file, are limited to
  2 GiB, and are marked as downloaded from the internet so Windows warns
  before you run them. Progress and the result are listed; either person can
  cancel.

The status line under the picture shows the frame rate, the delay from
capture to display, and the round trip to the other computer. "No answer
from the host for N s" appears when it has gone quiet.

### If the connection drops

If the connection breaks without anyone ending it (the other computer
crashed or lost its network, or nothing has been heard for 15 s), the window
says "Connection lost" and **tries again by itself**: after 2, 4, 8 and then
every 15 seconds, for up to two minutes. **Stop reconnecting** cancels it.
Each attempt is a new session, so the person at the other computer has to
**allow it again**. Clipboard sync is off again and a file that was being
sent has to be sent again. It does not retry when a different device
answers, when you were refused, or when either person ended the session.

If the connection is slower than the picture needs, the sharing computer
lowers the picture quality (and briefly holds the picture back) instead of
letting it fall seconds behind. Its window says so ("Video limited to …").
Turn this off with *Adapt quality to the connection* in Settings.

## Settings

The **Settings** section of the window; press **Save settings**. The file is
shown at the top of the window ("Settings file"). Defaults are safe: control,
clipboard and files off, other computers off.

| Setting | Meaning |
|---|---|
| Name shown to other devices | What prompts call this computer |
| Mouse and keyboard control, Clipboard sync, File transfer | What a session may do when sharing starts; changeable while sharing |
| Approval prompt timeout | Seconds before an unanswered prompt counts as "no" (default 30) |
| Idle timeout | **Not active.** Shown greyed out; sessions are not ended for being idle |
| Connection method | Automatic (direct first, relay if needed), direct only, relay only |
| Server, and its key fingerprint | For reaching devices on other networks; the fingerprint is required, see `docs/DEPLOYMENT.md` |
| Let other computers connect | Off by default; sharing then accepts this computer only |
| Direct connection port | 0 chooses one each time. A fixed port is easier to allow through a firewall |
| Let devices on the local network find this one | Answers "who is sharing?" on UDP 47821 while sharing and open to other computers |
| Maximum frames per second, Maximum bitrate | Upper limits for the picture |
| Adapt quality to the connection | Lower the picture rate when the connection falls behind |
| Logging | Level (after restart), whether to write a file, how many days to keep |

## Where things are

| What | Where |
|---|---|
| Settings | `%APPDATA%\RemoteControl\config\config.toml` (the window shows the exact path) |
| This device's key | `identity.key` next to it, **encrypted with your Windows account** (DPAPI): another account, or the file copied to another PC, cannot use it |
| Remembered devices | `trusted.toml` next to it |
| Logs | `%LOCALAPPDATA%\RemoteControl\data\logs`, one file per day, kept 14 days by default. They name devices, keys, addresses, when sessions began and ended and the size of files, never the screen, input, clipboard text or file names and contents |
| Received files | `Downloads\Remote Control` |
| The environment variable `RC_CONFIG_DIR` | Keeps settings, key and logs under one folder instead (for testing) |

Deleting `identity.key` gives the computer a new identity: devices that
remembered it, and servers that know its ID, will see a different device.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| "cannot connect … actively refused" | The other computer is not sharing, or *Let other computers connect* is off there, or the address/port is wrong |
| "Could not reach the device (…)" | The server is unreachable or the device is not registered with it; the message lists what was tried |
| "The device … is not the one you picked" | The device at that address has another key than the one you chose. Do not continue unless you can explain why |
| "the host is busy with another viewer" | Someone else is connected or being asked |
| "try again in a moment" | The person just refused or ignored a request; a short pause is enforced |
| "the host did not answer" / "declined" | Nobody allowed it within the timeout / they said no |
| Black or frozen picture | The other computer is on the lock screen or showing a secure prompt |
| Picture is blurry or slow | The connection cannot carry more; see the status line and the host's window |
| "This system's web view cannot decode video" | The WebView2 component is missing or too old |
| Windows blocks the installer | Unsigned installer, see "Install" |

## What it deliberately does not do

No unattended access (there is no way to connect when nobody is there to
allow it), no starting with Windows, no hidden window or tray-only mode, no
background service, no remote administration or software installation, no
sound, no audio capture, no sharing of more than one display at a time, no
recording.
