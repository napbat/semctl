/** Native file chips keep the host's spacing, colors and link treatment. */
export const RESULT_STYLES: readonly { name: string; css: string }[] = [{ name: "semctl-results", css: `
[data-role="semctl.matches"]{display:flex;flex-wrap:wrap;align-items:flex-start;min-width:0}
[data-role="semctl.match"]{flex:0 1 auto;min-width:0;max-width:100%}
[data-role="semctl.match.expanded"]{flex:0 0 100%;width:100%;min-width:0;max-width:100%}
[data-role="semctl.match"]>[data-role="omp.tool.chip"],[data-role="semctl.match.expanded"]>[data-role="omp.tool.chip"]{align-self:flex-start;width:max-content;max-width:100%;min-width:0;border-radius:var(--r-card);padding-left:8px;padding-right:8px;cursor:pointer}
[data-role="semctl.match.path"]{flex:0 1 auto;min-width:0}
[data-role="semctl.match.preview"]{min-width:0;padding:4px 0 6px 0}
[data-role="semctl.batch.heading"]{flex:0 0 100%;min-width:0}
[data-role="semctl.score"][data-style="bar"]{width:40px;min-width:40px;max-width:40px;flex:0 0 40px}
[data-role="semctl.score"] .sf-mt-track{height:4px}
[data-role="semctl.score"].unknown .sf-mt-track{background:var(--l3);border:0;box-shadow:none;mask-image:none}
` }];