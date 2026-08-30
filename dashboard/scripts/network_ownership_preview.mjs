// Isolated UI fixture: real markup/styles/controller, disposable in-memory IPC.
// No real identity, keychain, clipboard or network configuration is accessed.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const html = fs.readFileSync(path.join(root, 'index.html'), 'utf8');
const section = html.match(/<section id="network-ownership-settings"[\s\S]*?<\/section>/)[0];
const mock = `
var handlers = {};
var data = {mode:'managed',share:false,status:'ready',configured:true,local_interfaces_allowed:true,unix_supported:true,credential_saved:false};
window.ratspeakDeveloperModeEnabled = function(){return document.getElementById('fixture-developer').checked;};
window.rsConfirm = async function(){return true;};
window.RS = {
 listen:function(name,fn){(handlers[name] ||= []).push(fn);},
 copyText:async function(){return true;},
 invoke:async function(name,payload){
  if(name==='api_network_ownership') return data;
  if(name==='test_shared_instance') {
   if(payload.args.rpc_key==='bad') throw new Error('The shared instance rejected the RPC key. The current network has not been changed.');
   return {message:'Packet service and authenticated RPC are available. Nothing has been changed.'};
  }
  if(name==='set_network_ownership') {
   data=Object.assign({},data,payload.args,{rpc_key:undefined,credential_saved:payload.args.mode==='existing',local_interfaces_allowed:payload.args.mode==='managed'});
   (handlers.network_ownership||[]).forEach(function(fn){fn(data);});return data;
  }
  if(name==='import_shared_access') return JSON.parse(payload.content);
  if(name==='export_shared_access') return 'DISPOSABLE TEST ACCESS CONFIGURATION';
 }
};
document.getElementById('fixture-developer').addEventListener('change',function(){window.dispatchEvent(new CustomEvent('ratspeak-developer-mode-changed'));});
document.getElementById('fixture-reject').addEventListener('click',function(){data=Object.assign({},data,{mode:'existing',local_interfaces_allowed:false,status:'authentication_rejected'});(handlers.network_ownership||[]).forEach(function(fn){fn(data);});});
`;
const page = `<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/static/style.css"><title>Network ownership · UI fixture</title><style>body{display:block;overflow:auto;padding:24px}main{max-width:620px;margin:auto}header{margin-bottom:24px}.panel-body{padding:16px}.fixture-actions{display:flex;gap:12px;flex-wrap:wrap}</style></head><body><main><header><h2>Network settings</h2><p>Disposable UI test — no real network or credentials</p><div class="fixture-actions"><label><input id="fixture-developer" type="checkbox" checked> Developer Mode</label><button id="fixture-reject" class="nr-btn nr-btn-sm">Simulate rejected key</button></div></header><div id="network-owner-notice" class="network-owner-notice" hidden></div><div id="network-owner-interfaces" hidden></div><div class="view-grid-settings"><div class="panel-body">${section}<div class="settings-row"><span>Transport Mode</span><button id="transport-mode-select" class="selector-badge">OFF</button></div></div></div></main><script>${mock}</script><script src="/static/js/network_ownership.js"></script></body></html>`;
const assets = new Map([
    ['/static/style.css', ['static/style.css', 'text/css']],
    ['/static/js/network_ownership.js', ['static/js/network_ownership.js', 'application/javascript']],
]);
const server = http.createServer((request, response) => {
    const route = new URL(request.url, 'http://127.0.0.1').pathname;
    if (route === '/') { response.setHeader('Content-Type', 'text/html; charset=utf-8'); response.end(page); return; }
    const asset = assets.get(route);
    if (!asset) { response.writeHead(404); response.end(); return; }
    response.setHeader('Content-Type', asset[1]);
    response.end(fs.readFileSync(path.join(root, asset[0])));
});
server.listen(Number(process.env.RATSPEAK_UI_FIXTURE_PORT || 4173), '127.0.0.1', () => {
    console.log(`Disposable Network UI fixture: http://127.0.0.1:${server.address().port}`);
});
