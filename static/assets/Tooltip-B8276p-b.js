import{c as f,j as n,x}from"./index-DgnP7O5y.js";import{r,R as s}from"./vendor-react-BNNc6vJO.js";/**
 * @license lucide-react v0.303.0 - ISC
 *
 * This source code is licensed under the ISC license.
 * See the LICENSE file in the root directory of this source tree.
 */const y=f("FlaskConical",[["path",{d:"M10 2v7.527a2 2 0 0 1-.211.896L4.72 20.55a1 1 0 0 0 .9 1.45h12.76a1 1 0 0 0 .9-1.45l-5.069-10.127A2 2 0 0 1 14 9.527V2",key:"pzvekw"}],["path",{d:"M8.5 2h7",key:"csnxdl"}],["path",{d:"M7 16h10",key:"wp8him"}]]);function E({children:c,label:p,placement:u="top",delay:d=300,className:m}){const[t,o]=r.useState(!1),e=s.useRef(null),a=r.useId(),i=()=>{e.current=setTimeout(()=>o(!0),d)},l=()=>{e.current&&clearTimeout(e.current),o(!1)};s.useEffect(()=>()=>{e.current&&clearTimeout(e.current)},[]);const h=s.cloneElement(c,{"aria-describedby":t?a:void 0,onMouseEnter:i,onMouseLeave:l,onFocus:i,onBlur:l});return n.jsxs("span",{className:"glass-tooltip-wrapper",style:{position:"relative",display:"inline-flex"},children:[h,t&&n.jsx("span",{id:a,role:"tooltip",className:x("glass-tooltip",`glass-tooltip-${u}`,m),children:p})]})}export{y as F,E as T};
