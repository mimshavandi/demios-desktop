import { Cloud, Cpu } from "lucide-react";
import type { ModelBadgeInfo } from "../models";
import { Badge } from "./primitives";

const SMALL = { size: 12, strokeWidth: 1.5 } as const;

export function ModelBadge({ info }: { info: ModelBadgeInfo | null }) {
  if (info === null) return <Badge tone="warning">No model</Badge>;
  return info.local ? (
    <Badge tone="local" icon={<Cpu {...SMALL} aria-hidden="true" />}>
      {info.text}
    </Badge>
  ) : (
    <Badge icon={<Cloud {...SMALL} aria-hidden="true" />}>{info.text}</Badge>
  );
}
