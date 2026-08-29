import { useEffect, useRef, useState, type ReactNode } from 'react';

import type { TranscriptBlock } from '../client/contracts.ts';

const longContent = 2_000;
const followThreshold = 48;

export function Transcript({ blocks }: { blocks: TranscriptBlock[] }) {
  const viewport = useRef<HTMLDivElement>(null);
  const follow = useRef(true);

  useEffect(() => {
    const element = viewport.current;
    if (!follow.current || element === null) return;
    if (typeof element.scrollTo === 'function') element.scrollTo({ top: element.scrollHeight });
    else element.scrollTop = element.scrollHeight;
  }, [blocks]);

  return (
    <div
      aria-label="Conversation transcript"
      className="transcript"
      onScroll={(event) => {
        const element = event.currentTarget;
        follow.current = element.scrollHeight - element.scrollTop - element.clientHeight <= followThreshold;
      }}
      ref={viewport}
      role="log"
    >
      {blocks.map((block) => <TranscriptItem block={block} key={block.id} />)}
    </div>
  );
}

export function TranscriptItem({ block }: { block: TranscriptBlock }) {
  switch (block.type) {
    case 'user': return <Block label="You"><p>{block.text}</p>{block.has_image && <p>Image attached</p>}</Block>;
    case 'assistant': return <Block label="Assistant">{block.spans.map((span, index) => span.type === 'reasoning'
      ? <details className="reasoning" key={index}><summary>Reasoning</summary><BoundedText text={span.text} /></details>
      : <BoundedText key={index} text={span.text} />)}</Block>;
    case 'tool_call': return <Block label={`Tool: ${block.tool}`} status={block.output === null ? 'Running' : block.is_error ? 'Failed' : 'Completed'}><BoundedText text={block.args} />{block.output !== null && <BoundedText text={block.output} />}</Block>;
    case 'notice': return <aside aria-live="polite" className={`transcript-block notice notice-${block.level}`} role="status"><strong>Notice</strong><p>{block.message}</p></aside>;
    case 'error': return <aside aria-live="assertive" className="transcript-block error" role="alert"><strong>Error</strong><p>{block.message}</p>{block.recoverable && <p>Recovery is available.</p>}</aside>;
    case 'image': return <Block label="Image"><img alt="Assistant-provided image" src={`data:${block.media_type};base64,${block.data}`} /><p>{block.media_type}</p></Block>;
    case 'terminal': return <Block label="Turn status" status={block.outcome}><p>{block.message}</p></Block>;
    case 'subagent': return <Block label={`Subagent: ${block.name}`} status={block.state}><Transcript blocks={block.blocks} /></Block>;
  }
}

function Block({ children, label, status }: { children: ReactNode; label: string; status?: string }) {
  return <article className="transcript-block"><header><strong>{label}</strong>{status && <span className="block-status">{status}</span>}</header>{children}</article>;
}

export function BoundedText({ text }: { text: string }) {
  const [expanded, setExpanded] = useState(false);
  const bounded = text.length > longContent;
  return <div className="bounded-content"><pre>{bounded && !expanded ? `${text.slice(0, longContent)}…` : text}</pre>{bounded && <button aria-expanded={expanded} onClick={() => setExpanded((value) => !value)} type="button">{expanded ? 'Collapse' : 'Expand'}</button>}</div>;
}
