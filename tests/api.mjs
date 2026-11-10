import assert from 'node:assert/strict';
import {test} from 'node:test';
import {api} from '../assets/api.mjs';

const expired = {code:'session_expired',error:'연결이 만료되었습니다.'};
function responses(t,results) {
  const calls=[];
  t.mock.method(globalThis,'fetch',async (path,options) => {
    calls.push({path,options});
    const [status,body]=results.shift();
    return new Response(JSON.stringify(body),{status});
  });
  return calls;
}

test('an expired session refreshes the token and saves the unchanged label once',async t => {
  const calls=responses(t,[[403,expired],[200,{token:'fresh'}],[200,{revision:1}]]);
  const options={method:'PUT',headers:{'Content-Type':'application/json','X-Mimicmail-Token':'old'},body:JSON.stringify({label:'교수',color:'#b95f5b',revision:0})};
  assert.deepEqual(await api('/api/contact-label',options),{revision:1});
  assert.deepEqual(calls.map(call=>call.path),['/api/contact-label','/api/conversations','/api/contact-label']);
  assert.equal(calls[2].options.body,options.body);
  assert.equal(calls[2].options.headers.get('X-Mimicmail-Token'),'fresh');
  assert.equal(calls[2].options.headers.get('Content-Type'),'application/json');
  assert.equal(options.headers['X-Mimicmail-Token'],'old');
});

test('a second expired token stops instead of looping',async t => {
  const calls=responses(t,[[403,expired],[200,{token:'fresh'}],[403,expired]]);
  await assert.rejects(api('/api/contact-label',{method:'PUT'}),/연결이 만료/);
  assert.equal(calls.length,3);
});

test('origin failures and revision conflicts are never retried',async t => {
  const calls=responses(t,[[403,{error:'보관함 주소로 다시 접속하세요.'}],[409,{error:'다른 화면에서 변경되었습니다.'}]]);
  await assert.rejects(api('/api/contact-label',{method:'PUT'}),/보관함 주소/);
  await assert.rejects(api('/api/contact-label',{method:'PUT'}),/다른 화면/);
  assert.equal(calls.length,2);
});

test('failed session refresh leaves the pending write unsent',async t => {
  const calls=responses(t,[[403,expired],[503,{error:'잠시 후 다시 시도하세요.'}]]);
  await assert.rejects(api('/api/contact-label',{method:'PUT'}),/잠시 후/);
  assert.equal(calls.length,2);
});

test('a lost response is not retried because the write may have succeeded',async t => {
  const fetch=t.mock.method(globalThis,'fetch',async()=>{throw new TypeError('Failed to fetch');});
  await assert.rejects(api('/api/reply',{method:'POST'}),/Failed to fetch/);
  assert.equal(fetch.mock.callCount(),1);
});

test('page conflicts retain their HTTP status for refreshing the conversation',async t => {
  const calls=responses(t,[[409,{error:'대화 내용이 변경되었습니다.'}]]);
  await assert.rejects(api('/api/conversation?limit=50&revision=1'),error=>error.status===409&&error.message.includes('대화 내용'));
  assert.equal(calls.length,1);
});
