import{e as d,j as a}from"./index-ByeKy0tc.js";import{u as p}from"./vendor-i18n-DxfIEP3Y.js";import{C as b}from"./chevron-left-CI1ktzk0.js";import{C as f}from"./chevron-right-DawQVkxZ.js";/**
 * @license lucide-react v0.303.0 - ISC
 *
 * This source code is licensed under the ISC license.
 * See the LICENSE file in the root directory of this source tree.
 */const x=d("ChevronsLeft",[["path",{d:"m11 17-5-5 5-5",key:"13zhaf"}],["path",{d:"m18 17-5-5 5-5",key:"h8a8et"}]]);/**
 * @license lucide-react v0.303.0 - ISC
 *
 * This source code is licensed under the ISC license.
 * See the LICENSE file in the root directory of this source tree.
 */const j=d("ChevronsRight",[["path",{d:"m6 17 5-5-5-5",key:"xnjwq"}],["path",{d:"m13 17 5-5-5-5",key:"17xmmf"}]]);function N({page:n,totalPages:t,onChange:m,siblingCount:l=2}){const{t:e}=p();if(t<=1)return null;const s=[],r=t,h=Math.max(2,n-l),c=Math.min(r-1,n+l);s.push(1),h>2&&s.push(-1);for(let i=h;i<=c;i+=1)s.push(i);c<r-1&&s.push(-1),r>1&&s.push(r);const o=i=>{i>=1&&i<=t&&i!==n&&m(i)};return a.jsxs("nav",{className:"pagination-bar","aria-label":e("分页导航"),children:[a.jsx("button",{type:"button",className:"pagination-item","aria-label":e("首页"),disabled:n<=1,onClick:()=>o(1),children:a.jsx(x,{size:16,"aria-hidden":"true"})}),a.jsx("button",{type:"button",className:"pagination-item","aria-label":e("上一页"),disabled:n<=1,onClick:()=>o(n-1),children:a.jsx(b,{size:16,"aria-hidden":"true"})}),s.map((i,u)=>i===-1?a.jsx("span",{className:"pagination-ellipsis","aria-hidden":"true",children:"…"},`ellipsis-${u}`):a.jsx("button",{type:"button",className:`pagination-item pagination-num${i===n?" is-active":""}`,"aria-label":e("第 {{n}} 页",{n:i}),"aria-current":i===n?"page":void 0,onClick:()=>o(i),children:i},i)),a.jsx("button",{type:"button",className:"pagination-item","aria-label":e("下一页"),disabled:n>=t,onClick:()=>o(n+1),children:a.jsx(f,{size:16,"aria-hidden":"true"})}),a.jsx("button",{type:"button",className:"pagination-item","aria-label":e("末页"),disabled:n>=t,onClick:()=>o(t),children:a.jsx(j,{size:16,"aria-hidden":"true"})})]})}export{N as P};
