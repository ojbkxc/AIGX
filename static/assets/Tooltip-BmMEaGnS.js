import{c as l,j as r,w as f}from"./index-CfWfCplk.js";import{r as n,R as t}from"./vendor-react-BNNc6vJO.js";/**
 * @license lucide-react v0.303.0 - ISC
 *
 * This source code is licensed under the ISC license.
 * See the LICENSE file in the root directory of this source tree.
 */const M=l("FlaskConical",[["path",{d:"M10 2v7.527a2 2 0 0 1-.211.896L4.72 20.55a1 1 0 0 0 .9 1.45h12.76a1 1 0 0 0 .9-1.45l-5.069-10.127A2 2 0 0 1 14 9.527V2",key:"pzvekw"}],["path",{d:"M8.5 2h7",key:"csnxdl"}],["path",{d:"M7 16h10",key:"wp8him"}]]);/**
 * @license lucide-react v0.303.0 - ISC
 *
 * This source code is licensed under the ISC license.
 * See the LICENSE file in the root directory of this source tree.
 */const g=l("Settings2",[["path",{d:"M20 7h-9",key:"3s1dr2"}],["path",{d:"M14 17H5",key:"gfn3mx"}],["circle",{cx:"17",cy:"17",r:"3",key:"18b49y"}],["circle",{cx:"7",cy:"7",r:"3",key:"dfmy0x"}]]);function v({children:p,label:u,placement:d="top",delay:h=300,className:m}){const[s,o]=n.useState(!1),e=t.useRef(null),a=n.useId(),c=()=>{e.current=setTimeout(()=>o(!0),h)},i=()=>{e.current&&clearTimeout(e.current),o(!1)};t.useEffect(()=>()=>{e.current&&clearTimeout(e.current)},[]);const y=t.cloneElement(p,{"aria-describedby":s?a:void 0,onMouseEnter:c,onMouseLeave:i,onFocus:c,onBlur:i});return r.jsxs("span",{className:"glass-tooltip-wrapper",style:{position:"relative",display:"inline-flex"},children:[y,s&&r.jsx("span",{id:a,role:"tooltip",className:f("glass-tooltip",`glass-tooltip-${d}`,m),children:u})]})}export{M as F,g as S,v as T};
