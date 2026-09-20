// Exercise the shared-photo adapter against the real attachment state machine.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const source = fs.readFileSync(path.join(__dirname,'../../dashboard/static/js/lxmf.js'),'utf8');
function extract(name) {
    const start = source.indexOf('function '+name+'('), brace = source.indexOf('{',start);
    assert(start>=0); let depth=0;
    for(let n=brace;n<source.length;n++){if(source[n]==='{')depth++;if(source[n]==='}'&&--depth===0)return source.slice(start,n+1);}
    throw Error('unterminated '+name);
}
function fixture(disposition='still',prompt=true,choice='medium') {
    const calls=[],choices=[];
    const ctx={_pendingAttachmentToken:0,lxmfActiveContact:'recipient',lxmfPendingFile:null,
        renderPendingFile(){},showToast(){}, _attachmentCancelledError:()=>({cancelled:true}),
        _stageAttachmentBlob(){throw Error('Shared sources must never pass through WebView Blob staging');},
        _imageProfileLabel: x=>x, _imagePreviewUrl:()=> 'blob:tiny-preview',
        _chooseImageSize:async()=>{choices.push('size');return choice;},
        _chooseImageFileFallback:async()=>{choices.push('fallback');return choice;},
        clearPendingFile(){ctx._pendingAttachmentToken++;ctx.lxmfPendingFile=null;},
        RS:{invoke:async(name,args)=>{calls.push({name,args});
            if(name==='inspect_image_attachment_stage')return{disposition,should_prompt:prompt};
            if(name==='prepare_image_attachment_stage')return{file_name:'photo.jpg',mime:'image/jpeg',size:321,profile:args.args.profile};
            return{};
        }}
    };
    vm.createContext(ctx);
    for(const name of ['_isCurrentPendingAttachment','_finishPendingImageAsFile','_stageSelectedImage','attachSharedImage']) vm.runInContext(extract(name),ctx);
    return{ctx,calls,choices};
}
(async()=>{
    for(const [disposition,prompt,choice,expected] of [['still',true,'medium','prepare_image_attachment_stage'],['still',false,'actual','prepare_image_attachment_stage'],['animated',false,'file','mark_image_attachment_stage_as_file']]){
        const f=fixture(disposition,prompt,choice);
        const pending=f.ctx.attachSharedImage({name:'original.jpg',mime:'image/jpeg',size:2_000_000},Promise.resolve('native-token'));
        assert.equal(await pending.stage_promise,'native-token');
        assert(f.calls.some(c=>c.name===expected));
        assert.equal(pending.preparing,false);
        if(disposition==='still')assert.equal(pending.size,321);
    }
    const cancelled=fixture('still',true,null);
    const pending=cancelled.ctx.attachSharedImage({name:'photo.jpg'},Promise.resolve('cancel-token'));
    assert.equal(await pending.stage_promise,null);
    assert.equal(cancelled.ctx.lxmfPendingFile,null);
    assert(cancelled.calls.some(c=>c.name==='cancel_attachment_stage'));
    const stale=fixture();let resolve;
    const late=stale.ctx.attachSharedImage({name:'photo.jpg'},new Promise(r=>resolve=r));
    stale.ctx.clearPendingFile();resolve('late-token');
    assert.equal(await late.stage_promise,null);
    assert(stale.calls.some(c=>c.name==='cancel_attachment_stage'));
    assert(!stale.calls.some(c=>c.name==='inspect_image_attachment_stage'));
    console.log('Shared photo preparation: native staging, sizing, actual, file fallback, cancellation and stale replies passed');
})().catch(e=>{console.error(e);process.exitCode=1;});
