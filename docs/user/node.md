# Your Demios Node

A Node is Demios running as a service on a machine you own, such as a home PC, a Mac mini or a server. You pair this computer with it, and your chats and projects sync through it to your other paired devices. The Node is an early preview: this page covers what it does today, and the [limits](#current-limits) at the end.

There's no Demios account and no Demios server in this. Your devices talk to your Node directly.

## Where to run it

Run a Node only on a machine you control. It keeps a copy of your synced projects, chats and messages there, encrypted with a key in its secret store (see [Your data on the Node](#your-data-on-the-node)). Use full-disk encryption on the machine too.

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
- **A device you pair later**, or one that hasn't synced for more than 90 days, starts from a copy of the current data on the Node, not from every change ever made. Then it syncs changes as usual.

## How your data is protected

Sync is encrypted end to end between your paired devices and your Node.

- Each change is sealed on the device that makes it, with a key that only your paired devices and your Node hold, and signed with that device's own key.
- Devices and the Node also talk over an encrypted connection, pinned to the keys they checked when you paired. A device that isn't your Node can't take its place: the app stops and says so.
- Your Node checks each change and passes it on to your other devices. It keeps a change only until every device has it.
- The Node's log records connections and errors, never what your chats say.

### Your data on the Node

The Node keeps its own copy of your synced projects, chats and messages, so a new device can start from it. It stores them encrypted in its database, with a records key kept in its secret store. Someone who copies the database or a backup of it, without that key, can't read them.

- **Still readable in the database:**
  - record ids;
  - which messages belong to which chat;
  - sizes;
  - when each device changed something;
  - the list of devices.
- **Not protected:**
  - someone in control of the running Node;
  - someone who has both its disk and its secret store;
  - swap and crash dumps, which can hold decrypted data.

  Full-disk encryption on the Node's machine helps with these.
- **With `insecure-plaintext`**, the key sits in a plain file next to the database, so the encryption protects nothing.

To replace the records key, run this on the Node while it's running, as the same user:

```sh
dn node rotate-records-key --data-dir /var/lib/demios
```

It makes a new key and uses it for new changes straight away. It re-encrypts the stored records in batches while devices keep syncing, then deletes the old key. If it stops partway, run it again to carry on. A backup of the database made before the rotation still opens with the old key, if you kept a copy of the secret store from then.

If the secret store no longer has the records key, for example after the Node moved to another machine without its keychain, the Node refuses to start and says which key is missing. It never makes a new key in its place.

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

You can also remove one on the Node, for example a device that's lost or one that has forgotten the Node. While the Node runs, as the same user:

```sh
dn node devices --data-dir /var/lib/demios
dn node remove-device --data-dir /var/lib/demios <id>
```

`dn node devices` lists the paired devices with their ids. `dn node remove-device` removes the one you name.

Either way:

- The device can't connect or sync again. Changes it makes after it's removed are refused.
- The Node makes a new key and gives it only to the devices that remain, so the removed device can't read what syncs from now on.
- The removed device keeps its copy of your data, because it may be offline or lost. If you have it, use **Delete all local data** on it first.

You can't remove the Node itself this way. To delete it, see [Delete the Node](#delete-the-node).

## Unpair, forget or delete

- **Unpair this device:** in **Devices**, open this device's **More** menu and choose **Unpair this device**. The Node removes this computer, and this computer stops syncing. Your chats and projects stay here. This needs the Node to answer; if it doesn't, nothing changes and the app says so.
- **Forget this Node:** shown when the Node isn't answering or has refused this computer, for example after it was removed. This computer stops syncing with it and your data stays here. The Node still lists this computer until you run `dn node remove-device` on the Node, or remove it from another paired device.
- **Delete all local data** (Settings → General) also ends sync and deletes this computer's device keys. See [Privacy and your data](privacy.md). Nothing is deleted on your other devices. The Node still lists this computer until you run `dn node remove-device` on the Node, or remove it from another paired device.

To pair again after any of these, start over with `dn node pair`.

## Delete the Node

Stop the Node, then run this on its machine:

```sh
dn node delete-all --data-dir /var/lib/demios
```

- **Before it deletes anything,** it lists what it will delete and asks you to type the Node's name. You can pass `--confirm "<name>"` instead.
- **It deletes:**
  - the Node's keys from its secret store: its own key, the sync keys and the records keys;
  - its database;
  - `node.json`.
- **Then it removes the data folder,** if nothing else is left in it. Files it didn't make are kept, and it names them.
- **It refuses to run while the Node is running.**
- **Your paired devices keep their own copies.** On each one, open **Settings → Node** and choose **Forget this Node**.

Uninstalling the Node doesn't delete its data folder; `dn node delete-all` does.

## Current limits

- **Your devices reach the Node directly.** That means on its network, or over a VPN, a forwarded port or a proxy (see [Where to run it](#where-to-run-it)). There's no relay for devices away from home yet. Settings → Node shows **Relay: None**.
- **On Windows, the Node's own commands aren't there yet.** `dn node pair`, `devices`, `remove-device`, `delete-all` and `rotate-records-key` aren't available, and the Node turns away devices that aren't paired.
- **Pair from the Node only.** You can't pair another device from this computer yet; use `dn node pair`.
- **Not there yet:** a passphrase-protected secret store (`dn node unlock`), a QR code in the terminal and the phone app.
