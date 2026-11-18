/** Registered by NativeSurface, separately from the component to avoid runtime import cycles. */
export const GRAPH_APPEARANCE_STYLES: readonly { name: string; css: string }[] = [{ name: "semctl-graph-appearance", css: `
.sf-overlay[data-role="omp.overlay.semctl-appearance"]{backdrop-filter:none;background:transparent}
.sf-overlay[data-role="omp.overlay.semctl-appearance"]>.sf-ov-card{width:min(390px,calc(100vw - 32px));max-height:calc(100vh - 80px)}
[data-role="omp.overlay.semctl-appearance"] .sf-ov-body{overflow:hidden;min-width:0}
[data-role="semctl.appearance"]{min-width:0;font-size:12px}
[data-role="semctl.appearance.body"]{flex:1 1 auto;min-width:0;min-height:0;overflow-x:hidden;overflow-y:auto;max-height:min(380px,calc(100vh - 360px));overscroll-behavior:contain}
[data-role="semctl.appearance.row"]{display:flex;flex-direction:row;align-items:center;gap:8px;min-width:0}
[data-role="semctl.appearance.label"]{flex:0 0 68px;min-width:68px}
[data-role="semctl.appearance.options"]{flex:1 1 0;min-width:0;flex-wrap:nowrap}
[data-role="semctl.appearance.layouts"]{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));flex:1 1 0;min-width:0;gap:4px}
[data-role="semctl.appearance.layouts"]>:first-child{grid-column:span 2}
.semctl-appearance-option{display:flex;align-items:center;justify-content:center;flex:1 1 0;min-width:0;min-height:28px;padding:5px 8px;border:1px solid var(--l2);border-radius:var(--r-ctl);background:var(--l0);color:var(--t2);font:inherit;cursor:pointer;white-space:normal;text-align:center;overflow-wrap:anywhere}
.semctl-appearance-option:hover,.semctl-appearance-option:focus-visible{background:var(--l1);color:var(--t1);outline:1px solid var(--t2);outline-offset:1px}
.semctl-appearance-option[data-selected=true]{color:var(--t1);background:var(--l2);border-color:var(--t2);font-weight:600}
.semctl-appearance-option[data-active=true]{outline:1px solid var(--t2);outline-offset:1px}
[data-role="semctl.appearance.error"]{overflow-wrap:anywhere}
[data-role="semctl.appearance.help"]{min-height:3.5em;padding:7px 8px;border:1px solid var(--l1);border-radius:var(--r-ctl);background:var(--l0);line-height:1.4;overflow-wrap:anywhere}
@media (max-width:450px){[data-role="semctl.appearance.row"]{flex-direction:column;align-items:stretch;gap:4px}[data-role="semctl.appearance.label"]{flex:0 0 auto;min-width:0}[data-role="semctl.appearance.options"]{flex:0 1 auto;flex-wrap:wrap}}
` }];
