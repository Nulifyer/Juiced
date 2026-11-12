import { useState, type ReactNode } from "react";
import type { ArtifactSet } from "./artifactSetData";
import { bytes } from "./domain";
import { Badge, Button, HelpTip } from "./ui";

export function referenceSummary(references: string[], total: number) {
  return `Registered references: ${references.join(", ")}${total > references.length ? ` and ${total - references.length} more` : ""}`;
}

type Props = { sets: ArtifactSet[]; total: number; pager: ReactNode; disabled: boolean; onRegister: (artifact: string, set: ArtifactSet) => void; onRemove: (set: ArtifactSet) => void };
export function ArtifactSets({ sets, total, pager, disabled, onRegister, onRemove }: Props) {
  const [expanded, setExpanded] = useState("");
  return <section className="management-section" aria-label="Artifact sets">
    <div className="section-heading"><div className="page-title"><h2>Model sets</h2><HelpTip label="Model sets">A set groups the files one model needs, such as weights and a projector. Its files stay on disk until the set is removed.</HelpTip></div><span className="muted">{total}</span></div>
    {pager}
    {sets.map((set) => {
      const members = set.specification.members;
      const primary = members.find((member) => member.name === set.specification.primary)!;
      const blocked = disabled || set.active_holds > 0 || set.reference_count > 0;
      return <article className="item-row" key={set.id} aria-label={`Artifact set ${set.id}`}>
        <div className="row-heading"><code className="break-word">{set.id}</code><Badge tone={set.members_present ? "quiet" : "warning"}>{!set.members_present ? "Members unavailable" : members.length === 1 ? "Primary file" : `${members.length} members`}</Badge></div>
        <p className="field-help break-word">{set.specification.origin.importer_id} · {set.specification.origin.source_revision}</p>
        {set.reference_count > 0 && <p className="field-help break-word">{referenceSummary(set.references, set.reference_count)}</p>}
                {!set.members_present && <p className="inline-warning">A file in this set is missing or changed.</p>}
        {members.length > 1 && <p className="field-help">Multi-file sets can't be registered yet.</p>}
        <div className="action-row wrap">
          <Button disabled={disabled || members.length !== 1 || !set.members_present} onClick={() => onRegister(primary.artifact, set)}>Register model</Button>
          <Button onClick={() => setExpanded(expanded === set.id ? "" : set.id)} aria-expanded={expanded === set.id}>{expanded === set.id ? "Hide member files" : "View member files"}</Button>
          <Button variant="danger" disabled={blocked} onClick={() => onRemove(set)}>Remove set</Button>
        </div>
        {expanded === set.id && <div aria-label="Set member files">{members.map((member) => <div className="management-row" key={member.name}><strong>{member.name} · {member.role}</strong><p className="field-help break-word">{member.artifact}</p><p className="field-help">{bytes(member.bytes)}</p></div>)}</div>}
      </article>;
    })}
  </section>;
}
