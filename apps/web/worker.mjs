import {runBundle} from './runner.mjs';
self.onmessage=async({data})=>{
  try{
    const gpuPacing=data.webgl?new Int32Array(data.gpuPacing):null;
    let gpuSequence=0;
    const manifestURL=new URL(data.manifestURL,self.location.href);
    async function fetchBytes(url){const response=await fetch(url);if(!response.ok)throw Error(`Fetch ${url}: ${response.status}`);return response.arrayBuffer();}
    const manifest=JSON.parse(new TextDecoder().decode(await fetchBytes(manifestURL)));
    const [moduleBytes,imageBytes,hostBytes,files]=await Promise.all([
      fetchBytes(new URL(manifest.module,manifestURL)),fetchBytes(new URL(manifest.image,manifestURL)),fetchBytes(new URL(data.hostURL,self.location.href)),
      Promise.all((manifest.files??[]).map(async file=>({path:file.path,bytes:await fetchBytes(new URL(file.url,manifestURL))}))),
    ]);
    runBundle({manifest,moduleBytes,imageBytes,hostBytes,files,input:data.input,gpuOnly:Boolean(data.webgl),
      onFrame:frame=>self.postMessage({type:'frame',...frame},[frame.pixels.buffer]),
      ...(data.webgl?{onGpuFrame:frame=>{
        const sequence=gpuSequence++;
        self.postMessage({type:'gpu-frame',...frame,sequence},[frame.words.buffer]);
        Atomics.wait(gpuPacing,0,sequence);
      }}:{}),
      onStatus:status=>self.postMessage({type:'status',...status}),
      onDialog:dialog=>{self.postMessage({type:'dialog',...dialog});},
    });
  }catch(error){self.postMessage({type:'error',message:error.stack??String(error)});}
};
