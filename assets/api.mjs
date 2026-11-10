export async function api(path, options={}, retry=true) {
  const response = await fetch(path,options);
  const data = await response.json();
  if (retry && response.status===403 && data.code==='session_expired' && ['POST','PUT','DELETE'].includes(options.method)) {
    // A rejected token has no side effects, so this request can be retried once.
    const session = await api('/api/conversations',{signal:options.signal},false);
    const headers = new Headers(options.headers);
    headers.set('X-Mimicmail-Token',session.token);
    return api(path,{...options,headers},false);
  }
  if (!response.ok) throw new Error(data.error||'요청을 완료하지 못했습니다.');
  return data;
}
