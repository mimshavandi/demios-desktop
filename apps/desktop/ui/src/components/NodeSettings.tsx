import {
  BadgeAlert,
  CircleCheck,
  CircleOff,
  Clock,
  ExternalLink,
  Globe,
  Laptop,
  MoreHorizontal,
  Server,
  Smartphone,
  Trash2,
  Unlink,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type JoinStarted,
  type NodeDevice,
  type LicenceKind,
  type NodeLicence,
  type NodeOverview,
  type SyncStatus,
} from "../ipc/types";
import { ConfirmDialog } from "./ConfirmDialog";
import { Dialog } from "./Dialog";
import { DropdownMenu } from "./DropdownMenu";
import { Badge, Button, EmptyState, ICON, InlineMessage, Skeleton, TextField } from "./primitives";
import { useToast } from "./toast";

/** "03:12" today, otherwise "26 Nov, 03:12". */
export function whenText(ms: number, now = Date.now()): string {
  const d = new Date(ms);
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return new Date(now).toDateString() === d.toDateString()
    ? time
    : `${d.toLocaleDateString([], { day: "numeric", month: "short" })}, ${time}`;
}

/** "Syncing now", "Last synced 2 min ago", "Not synced yet". */
export function lastSyncedText(ms: number | null, now = Date.now()): string {
  if (ms === null) return "Not synced yet";
  const min = Math.floor((now - ms) / 60_000);
  if (min < 1) return "Last synced just now";
  if (min < 60) return `Last synced ${min} min ago`;
  return `Last synced ${whenText(ms, now)}`;
}

/** This device's row: where sync stands ("Syncing now", "Up to date · last synced …"). */
export function thisDeviceSyncText(sync: SyncStatus, now = Date.now()): string {
  switch (sync.state) {
    case "syncing":
      return "Syncing now";
    case "up_to_date":
      return `Up to date · ${lastSyncedText(sync.last_synced_at, now).replace("Last synced", "synced")}`;
    case "retrying":
      return "Trying again soon";
    case "stopped":
      return "Sync stopped";
    case "off":
      return lastSyncedText(sync.last_synced_at, now);
  }
}

/** "Sync stopped: your Node removed this computer. …" */
function stoppedText(error: string | null): string {
  if (error === null) return "Sync stopped.";
  return `Sync stopped: ${error.charAt(0).toLowerCase()}${error.slice(1)}`;
}

const KIND_ICON = { desktop: Laptop, node: Server, phone: Smartphone } as const;

/** Where to get Pro; opened in the browser only when the person asks. */
export const PRO_URL = "https://demios.io/pro";

const DAY_MS = 86_400_000;

/** "31 Dec 2027". */
export function dateText(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleDateString("en-GB", { day: "numeric", month: "short", year: "numeric" });
}

/** Whole days left, rounded up: "9 days left" on the ninth-to-last day. */
export function daysLeft(unixSeconds: number, now = Date.now()): number {
  return Math.max(0, Math.ceil((unixSeconds * 1000 - now) / DAY_MS));
}

function kindName(kind: LicenceKind): string {
  return kind === "pro" ? "Pro" : kind === "founding" ? "Founding" : kind === "alpha" ? "Alpha" : "Trial";
}

/** True when the licence stops sync, jobs and pairing on the Node. */
export function licenceBlocks(licence: NodeLicence | undefined): boolean {
  return licence !== undefined && licence.state !== "valid";
}

/** Why the Node stopped, for the error under its facts. */
function licenceStoppedText(licence: NodeLicence): string {
  switch (licence.state) {
    case "ended":
      return `Your Node's ${kindName(licence.kind)} licence ended on ${dateText(licence.ended_at)}. Sync and jobs on the Node have stopped. Everything stays on the Node and on your devices, and nothing was deleted.`;
    case "unusable":
      return `Your Node's licence can't be used: ${licence.reason}. Sync and jobs on the Node have stopped. Nothing was deleted.`;
    case "missing":
      return "Your Node has no licence. Sync and jobs on the Node have stopped. Nothing was deleted.";
    case "valid":
      return "";
  }
}

/** Settings › Node › Licence (#377; design: NodeSettings › Licence). */
function LicenceFact({ licence, onSeePro }: { licence: NodeLicence; onSeePro: () => void }) {
  const seePro = (
    <Button variant="ghost" size="sm" icon={<ExternalLink {...ICON} aria-hidden="true" />} onClick={onSeePro}>
      See Pro
    </Button>
  );
  if (licence.state === "valid" && licence.kind === "trial" && licence.expires_at !== null) {
    const left = daysLeft(licence.expires_at);
    const help = `Ends ${dateText(licence.expires_at).replace(/ \d{4}$/, "")}. Get Pro to keep the Node running after that.`;
    return (
      <dd>
        {left <= 3 ? (
          <Badge tone="warning" icon={<Clock size={12} strokeWidth={1.5} aria-hidden="true" />}>
            {left <= 1 ? "Trial ends today" : `Trial ends in ${left} days`}
          </Badge>
        ) : (
          `Trial, ${left} days left`
        )}
        <div className="dn-help">{help}</div>
        {seePro}
      </dd>
    );
  }
  if (licence.state === "valid") {
    return (
      <dd>
        {licence.expires_at === null
          ? kindName(licence.kind)
          : `${kindName(licence.kind)} until ${dateText(licence.expires_at)}`}
        <div className="dn-help">Licensed to {licence.licensee}</div>
      </dd>
    );
  }
  if (licence.state === "ended") {
    return (
      <dd>
        {kindName(licence.kind)}, ended {dateText(licence.ended_at)}
        <div className="dn-help">Licensed to {licence.licensee}</div>
      </dd>
    );
  }
  return <dd>{licence.state === "missing" ? "None" : "Can't be used"}</dd>;
}

type Load = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; view: NodeOverview };

/**
 * Settings › Node (ADR-0014; design: NodeSettings): pair this computer with a Demios Node,
 * see the Node's status and the devices paired with it, remove a device, or unpair.
 * - The overview is asked of the Node when the page opens and on Retry. When the Node
 *   doesn't answer, the page says since when; it never shows a frozen "Online".
 * - Removing a device can't be undone (its key is dropped at once), so it asks first.
 * - Who can connect (#308): the Node's own network by default, or a warning when it was set
 *   up to be reachable from outside. An older Node doesn't say, and the row isn't shown.
 */
export function NodeSettings({ api }: { api: Api }) {
  const toast = useToast();
  const [load, setLoad] = useState<Load>({ kind: "loading" });
  // Through the same native "Open link?" confirm as other links; nothing is sent by Demios.
  const openPro = async () => {
    try {
      await api.openLink(PRO_URL);
    } catch (e) {
      toast({ tone: "error", title: `Couldn't open the page. ${errorMessage(e)}` });
    }
  };
  const [joining, setJoining] = useState(false);
  const [removing, setRemoving] = useState<NodeDevice | null>(null);
  const [forgetting, setForgetting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pairRef = useRef<HTMLButtonElement>(null);

  const refresh = useCallback(async () => {
    setLoad({ kind: "loading" });
    try {
      setLoad({ kind: "ready", view: await api.nodeOverview() });
    } catch (e) {
      setLoad({ kind: "error", message: errorMessage(e) });
    }
  }, [api]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (load.kind === "loading") {
    return (
      <section className="card" aria-busy="true" aria-label="Node">
        <Skeleton />
        <Skeleton />
      </section>
    );
  }
  if (load.kind === "error") {
    return (
      <InlineMessage tone="error" action={<Button onClick={() => void refresh()}>Retry</Button>}>
        {load.message}
      </InlineMessage>
    );
  }
  const view = load.view;

  const confirmRemove = async () => {
    if (removing === null) return;
    setBusy(true);
    setError(null);
    try {
      const next = removing.this_device ? await api.nodeUnpair() : await api.nodeRemoveDevice(removing.id);
      setLoad({ kind: "ready", view: next });
      toast({
        tone: "success",
        title: removing.this_device
          ? "This computer is unpaired. Your data stays here."
          : `${removing.name} removed. It can't connect or sync again.`,
      });
      setRemoving(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const syncNow = async () => {
    setBusy(true);
    setError(null);
    try {
      setLoad({ kind: "ready", view: await api.nodeSyncNow() });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const confirmForget = async () => {
    setBusy(true);
    setError(null);
    try {
      setLoad({ kind: "ready", view: await api.nodeForget() });
      toast({ tone: "success", title: "This computer forgot the Node. Your data stays here." });
      setForgetting(false);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  if (view.paired === null) {
    return (
      <>
        <section className="card" aria-label="Node">
          <EmptyState
            title="No Node paired"
            action={
              <Button ref={pairRef} variant="primary" onClick={() => setJoining(true)}>
                Pair a Node
              </Button>
            }
          >
            A Node is Demios running on a machine you own, such as a home PC, a Mac mini or a
            server. It runs your jobs while this app is closed. It keeps your chats, keys and
            memory on that machine, unencrypted, so it can work while this app is closed. Only
            pair one you control.
          </EmptyState>
        </section>
        <JoinNodeDialog
          api={api}
          open={joining}
          onOpenChange={setJoining}
          returnFocusTo={pairRef}
          onJoined={(next) => {
            setLoad({ kind: "ready", view: next });
            setJoining(false);
            toast({ tone: "success", title: `Paired with ${next.paired?.name ?? "your Node"}.` });
          }}
        />
      </>
    );
  }

  const paired = view.paired;
  const status = view.status;
  const licence = status?.licence;
  const blocked = view.online && licenceBlocks(licence);
  return (
    <>
      <section className="card node-card" aria-labelledby="node-h">
        <h2 id="node-h">
          {paired.name}{" "}
          {blocked ? (
            <Badge tone="warning" icon={<BadgeAlert size={12} strokeWidth={1.5} aria-hidden="true" />}>
              {licence?.state === "ended" ? "Licence ended" : "Licence can't be used"}
            </Badge>
          ) : view.online ? (
            <Badge tone="success" icon={<CircleCheck size={12} strokeWidth={1.5} aria-hidden="true" />}>
              Online
            </Badge>
          ) : (
            <Badge tone="warning" icon={<CircleOff size={12} strokeWidth={1.5} aria-hidden="true" />}>
              Offline
            </Badge>
          )}
        </h2>
        {view.problem !== null && (
          <InlineMessage tone="error" action={<Button size="sm" onClick={() => void refresh()}>Retry</Button>}>
            {view.problem}
          </InlineMessage>
        )}
        {!view.online && view.problem === null && (
          <InlineMessage tone="warning" action={<Button size="sm" onClick={() => void refresh()}>Retry</Button>}>
            {paired.last_reached_at === null
              ? "Your Node isn't answering."
              : `Your Node isn't answering. Last reached ${whenText(paired.last_reached_at)}.`}{" "}
            Changes made here are kept and sync when it's back.
          </InlineMessage>
        )}
        {blocked && licence !== undefined && (
          <InlineMessage
            tone="error"
            action={
              <Button size="sm" onClick={() => void openPro()}>
                See Pro
              </Button>
            }
          >
            {licenceStoppedText(licence)}
            <div className="dn-help">
              To add a new licence, run <code>dn node licence add &lt;file&gt;</code> on the Node.
            </div>
          </InlineMessage>
        )}
        {!blocked && view.sync.state === "retrying" && view.sync.error !== null && (
          <InlineMessage tone="warning">{view.sync.error} Trying again soon.</InlineMessage>
        )}
        {!blocked && view.sync.state === "stopped" && (
          <InlineMessage
            tone="error"
            action={
              <Button size="sm" disabled={busy} onClick={() => void syncNow()}>
                Sync now
              </Button>
            }
          >
            {stoppedText(view.sync.error)}
          </InlineMessage>
        )}
        <dl className="node-facts">
          {status !== null && (
            <>
              <dt>Version</dt>
              <dd>{status.version}</dd>
            </>
          )}
          {licence !== undefined && (
            <>
              <dt>Licence</dt>
              <LicenceFact licence={licence} onSeePro={() => void openPro()} />
            </>
          )}
          <dt>Reached through</dt>
          <dd>{paired.address}</dd>
          {status?.reachable_from_outside !== undefined && (
            <>
              <dt>Who can connect</dt>
              <dd>
                {status.reachable_from_outside ? (
                  <>
                    <Badge tone="warning" icon={<Globe size={12} strokeWidth={1.5} aria-hidden="true" />}>
                      Reachable from outside your network
                    </Badge>
                    <div className="dn-help">
                      Anyone who can reach its port can try to connect. Only paired devices get in.
                    </div>
                  </>
                ) : (
                  "Devices on its network"
                )}
              </dd>
            </>
          )}
          {status !== null && (
            <>
              <dt>Secrets kept in</dt>
              <dd>
                {status.secret_store_label}{" "}
                {!status.protected_at_rest && <Badge tone="warning">Unprotected</Badge>}
              </dd>
            </>
          )}
          <dt>Relay</dt>
          <dd>None. Devices sync only when they reach the Node directly.</dd>
          <dt>Phone wake-ups</dt>
          <dd>Off</dd>
        </dl>
        {(!view.online || view.problem !== null) && (
          <div>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setError(null);
                setForgetting(true);
              }}
            >
              Forget this Node
            </Button>
          </div>
        )}
      </section>

      <section className="card" aria-labelledby="node-devices-h">
        <h2 id="node-devices-h">Devices</h2>
        {view.devices.length === 0 ? (
          <p className="dn-help">
            {view.online ? "No devices listed." : "The device list shows when your Node answers."}
          </p>
        ) : (
          <ul className="node-devices">
            {view.devices.map((d) => {
              const Icon = KIND_ICON[d.kind];
              return (
                <li key={d.id} className="row">
                  <div className="row-main node-device">
                    <Icon {...ICON} aria-hidden="true" />
                    <div>
                      <span className="row-title">{d.name}</span>{" "}
                      {d.this_device && <Badge>This device</Badge>}
                      {d.kind === "node" && <Badge>Node</Badge>}
                      <div className="dn-help">
                        {d.this_device ? thisDeviceSyncText(view.sync) : lastSyncedText(d.last_synced_at)}
                      </div>
                    </div>
                  </div>
                  {d.kind !== "node" && (
                    <div className="row-actions">
                      <DropdownMenu
                        label={`More for ${d.name}`}
                        trigger={<MoreHorizontal {...ICON} aria-hidden="true" />}
                        entries={[
                          d.this_device
                            ? {
                                kind: "item",
                                label: "Unpair this device",
                                icon: <Unlink {...ICON} aria-hidden="true" />,
                                danger: true,
                                failure: "Couldn't unpair",
                                run: () => {
                                  setError(null);
                                  setRemoving(d);
                                },
                              }
                            : {
                                kind: "item",
                                label: "Remove device",
                                icon: <Trash2 {...ICON} aria-hidden="true" />,
                                danger: true,
                                failure: "Couldn't remove the device",
                                run: () => {
                                  setError(null);
                                  setRemoving(d);
                                },
                              },
                        ]}
                      />
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        )}
        <p className="dn-help">
          To pair another device, run <code>dn node pair</code> on the Node and open its link on
          the device.
        </p>
      </section>

      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(o) => {
          if (!o) setRemoving(null);
        }}
        title={removing?.this_device ? "Unpair this device?" : `Remove ${removing?.name ?? "device"}?`}
        confirmLabel={removing?.this_device ? "Unpair this device" : "Remove device"}
        busy={busy}
        error={error}
        onConfirm={() => void confirmRemove()}
      >
        <p>
          {removing?.this_device
            ? "This device stops syncing. Your data stays here."
            : "It won't be able to connect or sync again. Its copy of your data stays on it, because it may be offline or lost. If you have it, use Delete all on it first."}
        </p>
      </ConfirmDialog>
      <ConfirmDialog
        open={forgetting}
        onOpenChange={setForgetting}
        title={`Forget ${paired.name}?`}
        confirmLabel="Forget this Node"
        busy={busy}
        error={error}
        onConfirm={() => void confirmForget()}
      >
        <p>
          This computer stops syncing with it. Your data stays here. The Node still lists this
          computer until you run <code>dn node remove-device</code> on the Node, or remove it from
          another paired device.
        </p>
      </ConfirmDialog>
    </>
  );
}

type Step =
  | { kind: "link" }
  | { kind: "connecting" }
  | { kind: "numbers"; started: JoinStarted };

/**
 * The join dialog (design: PairDevice, "Join a Node"): paste the link from `dn node pair`,
 * then compare the check numbers and confirm on the Node. Any mismatch stops pairing, and
 * nothing is shared.
 */
export function JoinNodeDialog({
  api,
  open,
  onOpenChange,
  onJoined,
  returnFocusTo,
}: {
  api: Api;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onJoined: (view: NodeOverview) => void;
  returnFocusTo?: React.RefObject<HTMLElement | null>;
}) {
  const [link, setLink] = useState("");
  const [step, setStep] = useState<Step>({ kind: "link" });
  const [error, setError] = useState<string | null>(null);
  // Answers that arrive after the dialog closed are ignored.
  const live = useRef(0);

  useEffect(() => {
    if (open) {
      setLink("");
      setStep({ kind: "link" });
      setError(null);
    }
    live.current += 1;
  }, [open]);

  const close = (stopJoin: boolean) => {
    if (stopJoin) void api.nodeJoinCancel();
    onOpenChange(false);
  };

  const connect = async () => {
    const token = live.current;
    setError(null);
    setStep({ kind: "connecting" });
    try {
      const started = await api.nodeJoinBegin(link.trim());
      if (token !== live.current) return;
      setStep({ kind: "numbers", started });
      // No time limit: the person is comparing the numbers on the Node.
      const view = await api.nodeJoinWait();
      if (token !== live.current) return;
      onJoined(view);
    } catch (e) {
      if (token !== live.current) return;
      setError(errorMessage(e));
      setStep({ kind: "link" });
    }
  };

  const mismatch = () => {
    live.current += 1;
    void api.nodeJoinCancel();
    setStep({ kind: "link" });
    setError("Pairing stopped. Nothing was shared. Start again with a new link.");
  };

  const numbers = step.kind === "numbers" ? step.started.numbers : null;
  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        if (!o) close(step.kind !== "link");
      }}
      title={numbers === null ? "Pair a Node" : "Check the numbers"}
      size="medium"
      testId="join-node-dialog"
      {...(returnFocusTo !== undefined ? { returnFocusTo } : {})}
    >
      {numbers === null ? (
        <form
          className="dn-dialog-body"
          onSubmit={(e) => {
            e.preventDefault();
            void connect();
          }}
        >
          <TextField
            label="Pairing link"
            placeholder="demios://pair?…"
            value={link}
            autoFocus
            onChange={(e) => setLink(e.target.value)}
            help={
              <>
                On the Node, run <code>dn node pair</code>. It shows a link that works for 10 minutes.
              </>
            }
          />
          {step.kind === "connecting" && (
            <p className="dn-help" role="status">
              Connecting…
            </p>
          )}
          {error !== null && <InlineMessage tone="error">{error}</InlineMessage>}
          <div className="dn-dialog-actions">
            <Button variant="ghost" onClick={() => close(false)}>
              Cancel
            </Button>
            <Button
              type="submit"
              variant="primary"
              loading={step.kind === "connecting"}
              disabled={link.trim() === "" || step.kind === "connecting"}
            >
              Connect
            </Button>
          </div>
        </form>
      ) : (
        <div className="dn-dialog-body">
          <p className="pd-numbers" aria-label={numbers.join(", ")} data-testid="check-numbers">
            {numbers.join(" ")}
          </p>
          <p>
            Your Node shows the same numbers. If they match, confirm on the Node. If not, stop:
            someone may be between this device and your Node.
          </p>
          <p className="pd-live" role="status">
            Waiting for you to confirm on the Node…
          </p>
          <div className="dn-dialog-actions">
            <Button variant="ghost" onClick={() => close(true)}>
              Cancel
            </Button>
            <Button onClick={mismatch}>They don't match</Button>
          </div>
        </div>
      )}
    </Dialog>
  );
}
