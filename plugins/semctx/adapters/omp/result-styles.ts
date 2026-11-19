/** Native file chips keep the host's spacing, colors and link treatment. Motion runs only under data-motion=full. */
export const RESULT_STYLES: readonly { name: string; css: string }[] = [{ name: "semctl-results", css: `
/* One ordered column keeps rank order readable. The column takes the widest chip, so scores and checks align at the right.
   The graph icon takes a narrow second column on the first row, under the header's match count, so it adds no row. */
.sf[data-role="semctl.matches"]{display:grid;grid-template-columns:fit-content(calc(100% - 32px)) 24px;justify-content:space-between;align-items:start;column-gap:8px;row-gap:2px;min-width:0}
.sf[data-role="semctl.matches"]>*{grid-column:1}
[data-role="semctl.match"]{min-width:0;max-width:100%}
[data-role="semctl.match.expanded"]{width:100%;min-width:0;max-width:100%}
[data-role="semctl.match"]>[data-role="omp.tool.chip"],[data-role="semctl.match.expanded"]>[data-role="omp.tool.chip"]{align-self:flex-start;width:100%;max-width:100%;min-width:0;box-sizing:border-box;border-radius:var(--r-card);padding-left:8px;padding-right:8px;cursor:pointer}
[data-role="semctl.match.path"]{flex:1 1 auto;min-width:0}
[data-role="semctl.match.preview"]{min-width:0;padding:4px 0 6px 0}
[data-role="semctl.batch.heading"]{min-width:0;padding-top:4px}
[data-role="semctl.matches"]>[data-role="semctl.match.graph"]{grid-column:2;grid-row:1;align-self:center;width:24px;height:24px;min-width:24px}
[data-role="semctl.score"][data-style="bar"]{width:40px;min-width:40px;max-width:40px;flex:0 0 40px;display:flex;align-items:center;align-self:center;margin-left:6px}
[data-role="semctl.score"] .sf-mt-track{height:4px;border-radius:2px;background:var(--l2);overflow:hidden}
[data-role="semctl.score"] .sf-mt-fill{border-radius:2px;background:var(--sf-accent,var(--accent))}
[data-role="semctl.score"].unknown .sf-mt-track{background:var(--l3);border:0;box-shadow:none;mask-image:none}
[data-role="semctl.matches"][data-motion=full]>[data-role="semctl.match"],[data-role="semctl.matches"][data-motion=full]>[data-role="semctl.match.expanded"]{animation:semctl-match-arrive 220ms var(--ease,ease-out) both}
[data-role="semctl.matches"][data-motion=full]>:nth-child(2){animation-delay:30ms}
[data-role="semctl.matches"][data-motion=full]>:nth-child(3){animation-delay:60ms}
[data-role="semctl.matches"][data-motion=full]>:nth-child(4){animation-delay:90ms}
[data-role="semctl.matches"][data-motion=full]>:nth-child(5){animation-delay:120ms}
[data-role="semctl.matches"][data-motion=full]>:nth-child(6){animation-delay:150ms}
[data-role="semctl.matches"][data-motion=full]>:nth-child(7){animation-delay:180ms}
[data-role="semctl.matches"][data-motion=full]>:nth-child(n+8){animation-delay:210ms}
[data-role="semctl.matches"][data-motion=full] [data-role="omp.tool.chip"]{transition:background-color 100ms ease-out}
[data-role="semctl.matches"][data-motion=full] [data-role="semctl.score"] .sf-mt-fill{transition:width 240ms var(--ease,ease-out),left 240ms var(--ease,ease-out)}
[data-role="semctl.matches"][data-motion=reduced] [data-role="omp.tool.chip"],[data-role="semctl.matches"][data-motion=reduced] [data-role="semctl.score"] .sf-mt-fill{transition:none}
@keyframes semctl-match-arrive{from{opacity:0;transform:translateY(3px)}to{opacity:1;transform:none}}
@media (prefers-reduced-motion:reduce){[data-role="semctl.matches"] [data-role="omp.tool.chip"],[data-role="semctl.matches"] [data-role="semctl.score"] .sf-mt-fill,[data-role="semctl.matches"]>[data-role="semctl.match"],[data-role="semctl.matches"]>[data-role="semctl.match.expanded"]{transition:none;animation:none}}
` }];
