# Your Demios Node

A Node is Demios running as a service on a machine you own, such as a home PC, a Mac mini or a server. You pair this computer with it, and your chats and projects sync through it to your other paired devices. The Node is an early preview: this page covers what it does today, and the [limits](#current-limits) at the end.

There's no Demios account and no Demios server in this. Your devices talk to your Node directly.

## Where to run it

Run a Node only on a machine you control. It keeps its database on that machine without encrypting it (see [Current limits](#current-limits)), so use full-disk encryption on it.

The Node accepts devices on its own network only: private, link-local and loopback addresses. It turns away every other address before any connection is set up, and never asks your router to open a port.

- **On the same network:** nothing to set up.
- **Over your own VPN:** a VPN that gives your devices private addresses, such as 10.x or 192.168.x, works as it is. An overlay network such as Tailscale uses addresses the Node counts as outside its network, so set the Node up with `--reachable-from-outside` (see below).
- **Through a forwarded port or a proxy:** this also needs `--reachable-from-outside`, and anyone who can reach the port can then try to connect. Only paired devices get in. A VPN is the safer choice.

If a proxy or port forwarder runs on the Node's own machine, such as a reverse proxy or a container's published port, every connection seems to come from the machine itself. Then the Node can't tell who is outside, so treat it as reachable from outside.

## Set up and start the Node

The Node is part of the `dn` command-line tool (see [The dn command-line tool](dn-cli.md)). On the Node's machine:

```sh
dn node init --data-dir /var/lib/demios --name "Home server" --secrets systemd-credential:demios-node
dn node run --data-dir /var/lib/demios
```

- `dn node init` creates the data folder, readable by your user only. `dn node run` refuses to start if other users can read it.
- `--name` is the name your devices show for the Node.
- `--listen` sets the address and port the Node listens on. Without it, the Node listens on port 7443 on all of the machine's addresses. It doesn't change who may connect.
- `--reachable-from-outside` lets devices connect from any address, not only the Node's own network. Use it for an overlay VPN, a forwarded port or a proxy. Without it, the Node accepts devices on its network only.
- `--time-zone` sets the time zone for background jobs, such as `Europe/Berlin`. Without it, the Node uses the machine's time zone.
- `dn node run` runs in the foreground, so a service manager such as systemd can start it. Ctrl+C stops it.

## Your licence

The Node is part of Demios Pro. The desktop app needs no licence, and neither does a Relay.

- **Add your licence file** on the Node's machine, as the user that runs the Node:

  ```sh
  dn node licence add --data-dir /var/lib/demios ~/demios-licence.json
  ```

  It checks the file, then keeps it in the Node's secret store. It says what it added, for example "Licence added: Pro for Jo Rivera until 31 Dec 2027." If the Node is running, it uses the licence at once.
- **The check is offline.** The Node checks the file's signature itself, when it starts and at least once a day. It never sends anything to Demios to check a licence.
- **Without a licence,** or with a file that can't be used, `dn node run` doesn't start and `dn node pair` refuses. Each says what's missing and how to fix it. Nothing on the Node is changed.
- **When a licence ends,** the Node stops syncing, running jobs and pairing, and still answers your devices, so **Settings → Node** shows **Licence ended** and says why. Your data stays on the Node and on every device, nothing is deleted, and `dn node delete-all` still works. Add a new licence to go on.
- **Your computer's clock matters.** The Node remembers the latest time it has seen. If the clock goes back by more than a day, it asks you to check your computer's date and time instead of accepting the licence.
- **Settings → Node** shows the licence: "Pro until …", or for a trial how many days are left.

## Where the Node keeps secrets

`--secrets` chooses where the Node keeps its keys. It opens exactly that store, or refuses to start and says why. It never falls back to another one.

- `keychain`: the system keychain (macOS, or Secret Service on Linux), or Windows Credential Manager under the service account.
- `systemd-credential:<name>`: an encrypted file, `secrets.enc` in the data folder. Its key comes from a systemd credential, which systemd can seal to the machine's TPM.
- `container-secret:<name>`: the same encrypted file, with its key from a Docker or Podman secret.
- `key-file:<path>`: the same encrypted file, with its key in a file you keep on another disk. The Node refuses a key file on the same disk as the data folder, because a stolen disk would carry both.
- `insecure-plaintext`: an unencrypted file, for test setups only. Every paired device marks it **Unprotected**.

For an encrypted file, the key is 32 random bytes, for example from `openssl rand -hex 32`.

The Node also refuses to start if API keys or tokens are set in environment variables, such as `OPENAI_API_KEY`. Keep them in its secret store instead.

On this computer, its own device keys are kept in your system keychain.

## Pair this computer with the Node

You need the Node running, and a terminal on the Node's machine as the same user that runs it.

1. On the Node, run:

   ```sh
   dn node pair --data-dir /var/lib/demios --address 192.168.1.20:7443
   ```

   `--address` is the address your devices use to reach the Node. You can leave it out if `--listen` names one address. `dn node pair` shows a link that starts with `demios://pair`. It works once, for 10 minutes.
2. In the app, open **Settings → Node**, choose **Pair a Node**, paste the link and choose **Connect**.
3. The app and `dn node pair` both show 12 check digits, in three groups of four. Compare them.
4. If they match, type `yes` in `dn node pair`. The app says **Paired with** and the Node's name.

If the digits don't match, choose **They don't match** in the app and type `no` in `dn node pair`. Someone may be between this computer and your Node. Nothing is added unless you type `yes`. Typing anything else, closing `dn node pair` or letting the link expire adds nothing.

The Node runs one pairing at a time. After 5 wrong attempts with a link, that link stops working. To pair another device, run `dn node pair` again for a new link.

## What syncs

- **Synced:** chats, with their titles and messages, and projects, with their names and instructions. Whatever is already on this computer syncs once you pair it.
- **Deleting** a chat or project on one device deletes it on the others too.
- **Not synced:** artifacts, files you added to projects, settings, models, tool servers, skills and provider keys. Each device keeps its own.
- **If two devices rename the same chat** before they sync, both end up with the same name.

## How your data is protected

Sync is encrypted end to end between your paired devices and your Node.

- Each change is sealed on the device that makes it, with a key that only your paired devices and your Node hold, and signed with that device's own key.
- Devices and the Node also talk over an encrypted connection, pinned to the keys they checked when you paired. A device that isn't your Node can't take its place: the app stops and says so.
- Your Node passes sealed changes on to your other devices. It keeps them sealed and doesn't add them to chats of its own.
- The Node's log records connections and errors, never what your chats say.

## Sync in Settings → Node

Once paired, this computer syncs about once a minute while the app is open. **Settings → Node** shows the Node's name, whether it's online, its version, the address it's reached through, who can connect and where it keeps secrets. **Who can connect** says **Devices on its network**, or **Reachable from outside your network** if the Node was set up with `--reachable-from-outside`.

Your row in **Devices** says where sync stands:

- **Syncing now:** a sync is running.
- **Up to date:** the last sync worked, with when it ran. The next one starts on its own.
- **Trying again soon:** the last sync failed, and the reason shows above. The app tries again after a short pause that grows each time.
- **Sync stopped:** sync failed 5 times in a row, or failed in a way that waiting won't fix, such as this computer being removed. The reason shows above. Fix it, then choose **Sync now**.

The other devices' rows say when each last reached the Node.

If the Node doesn't answer, the page says **Your Node isn't answering**, with when it last answered. Changes you make here are kept and sync when it's back.

## Remove a device

In **Devices**, open a device's **More** menu (the … button) and choose **Remove device**.

- The device can't connect or sync again. Changes it makes after it's removed are refused.
- The Node makes a new key and gives it only to the devices that remain, so the removed device can't read what syncs from now on.
- The removed device keeps its copy of your data, because it may be offline or lost. If you have it, use **Delete all local data** on it first.

You can't remove the Node itself from a device.

## Unpair, forget or delete

- **Unpair this device:** in **Devices**, open this device's **More** menu and choose **Unpair this device**. The Node removes this computer, and this computer stops syncing. Your chats and projects stay here. This needs the Node to answer; if it doesn't, nothing changes and the app says so.
- **Forget this Node:** shown when the Node isn't answering or has refused this computer, for example after it was removed. This computer stops syncing with it and your data stays here. The Node still lists this computer until you remove it from another device.
- **Delete all local data** (Settings → General) also ends sync and deletes this computer's device keys. See [Privacy and your data](privacy.md). Nothing is deleted on your other devices. The Node still lists this computer until you remove it from another device.

To pair again after any of these, start over with `dn node pair`.

## Current limits

- **A new device replays every change.** Pairing a device sends it every change your devices have made since the first one paired, so the first sync can take a while with a long history.
- **Your devices reach the Node directly.** That means on its network, or over a VPN, a forwarded port or a proxy (see [Where to run it](#where-to-run-it)). There's no relay for devices away from home yet. Settings → Node shows **Relay: None**.
- **On Windows, a Node can't pair devices yet.** `dn node pair` isn't available, and the Node turns away devices that aren't paired.
- **Pair from the Node only.** You can't pair another device from this computer yet; use `dn node pair`.
- **The Node's database isn't encrypted** on disk. Use full-disk encryption on the Node's machine.
- **Not there yet:** a passphrase-protected secret store (`dn node unlock`), a QR code in the terminal and the phone app.
