const $ = id => document.getElementById(id);
const state = {data:null, current:null, messages:[], timeline:[], reference:null, creating:false, query:'', replies:false, label:'', labelSaving:false, deleting:null, deleteSaving:false, listRequest:0, chatRequest:0, editing:null, dirty:false, saving:false};
const requests = new Map();
const icon = name => { const svg=document.createElementNS('http://www.w3.org/2000/svg','svg'); const use=document.createElementNS(svg.namespaceURI,'use'); use.setAttribute('href',`#${name}-icon`);svg.append(use);svg.setAttribute('aria-hidden','true');return svg; };
function el(tag, cls, text){const node=document.createElement(tag);if(cls)node.className=cls;if(text!==undefined)node.textContent=text;return node;}
function button(label,cls,fn){const b=el('button',cls,label);b.type='button';b.addEventListener('click',fn);return b;}
function initials(name){return name.replace(/[<>"\s]/g,'').slice(0,2);}
function shortDate(date){return date ? new Intl.DateTimeFormat('ko-KR',{timeZone:'Asia/Seoul',month:'numeric',day:'numeric'}).format(new Date(date)) : '날짜 미상';}
function fullDate(date){return date ? new Intl.DateTimeFormat('ko-KR',{timeZone:'Asia/Seoul',year:'numeric',month:'long',day:'numeric',weekday:'short'}).format(new Date(date)) : '날짜 미상';}
function clockTime(date){return date ? new Intl.DateTimeFormat('ko-KR',{timeZone:'Asia/Seoul',hour:'2-digit',minute:'2-digit',hour12:false}).format(new Date(date)) : '';}
function avatar(name,index=0){return el('div',`avatar tone-${index%4}`,initials(name));}
function sameContact(a,b){return a&&b&&a.owner===b.owner&&a.recipient===b.recipient;}
function contactKey(contact){return JSON.stringify([contact.owner,contact.recipient]);}
function highlight(node,term){
  const text=node.textContent,index=text.indexOf(term);
  if(index<0)return null;
  const mark=el('mark','search-match',text.slice(index,index+term.length));
  node.replaceChildren(document.createTextNode(text.slice(0,index)),mark,document.createTextNode(text.slice(index+term.length)));
  return mark;
}
function clearSearchMatch(){for(const mark of $('timeline').querySelectorAll('mark.search-match')){const parent=mark.parentNode;mark.replaceWith(document.createTextNode(mark.textContent));parent.normalize();}}
function showSearchMatch(contact){
  clearSearchMatch();
  const match=contact.search_match;if(!match)return;
  const index=state.timeline.findIndex(item=>item.keys.includes(match.key));if(index<0)return;
  const row=$('timeline').querySelector(`[data-message-index="${index}"]`);
  const target=match.field==='body'?row.querySelector('.message-body'):row.previousElementSibling;
  const mark=highlight(target,match.term);if(!mark)return;
  if(match.field==='body'){
    target.classList.add('expanded');const toggle=target.nextElementSibling;
    toggle.textContent='접기';toggle.setAttribute('aria-expanded','true');updateMessageToggles();
  }
  mark.scrollIntoView({block:'center'});
}
function error(message){$('chat-error').textContent=message;$('chat-error').hidden=!message;}
let toastTimer;
function toast(message){$('toast').textContent=message;$('toast').hidden=false;clearTimeout(toastTimer);toastTimer=setTimeout(()=>$('toast').hidden=true,3500);}
async function api(path, options={}){const response=await fetch(path,options);const data=await response.json();if(!response.ok)throw new Error(data.error||'요청을 완료하지 못했습니다.');return data;}

async function loadConversations({refreshChat=false}={}){
  const request=++state.listRequest;
  $('list-status').textContent='불러오는 중…';
  $('refresh').disabled=true;
  try{
    const data=await api(`/api/conversations?${new URLSearchParams({q:state.query,replies:state.replies?'1':'0',label:state.label})}`);
    if(request!==state.listRequest)return;
    state.data=data;$('new-chat').disabled=false;
    $('library-summary').textContent=`보낸 메일 ${data.total_mails}통 · ${data.total_conversations}개의 대화`;
    const selected=data.items.find(item=>sameContact(item,state.current));
    if(selected)state.current=selected;
    $('filter-label').replaceChildren(new Option('모든 직급',''),...data.labels.map(label=>new Option(label,label)));
    $('filter-label').value=state.label;
    $('account-email').textContent=data.owners.join(', ');
    $('list-status').textContent=state.query?`검색 결과 ${data.items.length}개`:'';
    renderContacts();error('');
    if(!state.current&&data.items.length&&!matchMedia('(max-width:700px)').matches)await openConversation(data.items[0]);
    else if(refreshChat&&state.current)await openConversation(state.current,true);
  }catch(e){if(request===state.listRequest){$('list-status').textContent='목록 로드 실패 · 새로고침하세요.';error(e.message);}}
  finally{if(request===state.listRequest)$('refresh').disabled=false;}
}
function renderContacts(){
  const list=$('conversations');list.replaceChildren();
  state.data.items.forEach((item,index)=>{
    const row=button('',`conversation-row${sameContact(item,state.current)?' selected':''}`,()=>openConversation(item));
    row.setAttribute('aria-label',`${item.name}, 보낸 메일 ${item.count}통${item.reply_count?', 답장·인용 있음':''}`);
    row.setAttribute('aria-current',sameContact(item,state.current)?'true':'false');
    row.append(avatar(item.name,index));
    const content=el('div'),top=el('div','row-top');top.append(el('span','row-name',item.name),el('time','row-time',shortDate(item.latest)));
    const name=top.firstChild;if(item.label)name.append(el('span','rank-chip',item.label));
    const preview=el('p','row-subject',item.search_match?.preview||item.preview||item.subject||(item.registered?'새 대화':'(본문 없음)'));
    if(item.search_match)highlight(preview,item.search_match.term);
    content.append(top,preview);
    const bottom=el('div','row-bottom');
    if(item.reply_count){const badge=el('span','reply-badge');badge.append(icon('reply'),document.createTextNode('답장·인용'));bottom.append(badge);}

    bottom.append(el('span','row-count',`${item.count}통`));content.append(bottom);row.append(content);list.append(row);
  });
  if(!state.data.items.length)list.append(el('div','empty-list',state.data.total_mails?'검색 결과 없음':'메일 없음'));
}
async function openConversation(contact,keepScroll=false){
  document.querySelector('.app').classList.add('show-chat');
  if(sameContact(contact,state.current)&&state.reference&&!keepScroll){showSearchMatch(contact);return;}
  const request=++state.chatRequest;const oldScroll=$('timeline').scrollTop;
  state.current=contact;renderContacts();
  $('contact-name').textContent=contact.name;
  $('contact-email').textContent=contact.recipient;$('contact-email').hidden=false;
  $('contact-avatar').textContent=initials(contact.name);
  $('role-tag').textContent=contact.label||'직급 지정';$('role-tag').disabled=false;
  $('reply-note').textContent=contact.reply_count?'인용된 답장 포함':'';
  state.reference=null;$('copy-prompt').disabled=true;$('prompt-request').disabled=true;
  $('prompt-request').value=requests.get(contactKey(contact))||'';
  if(!keepScroll){$('timeline').replaceChildren(...Array.from({length:3},()=>el('div','loading-line')));}
  try{
    const data=await api(`/api/conversation?${new URLSearchParams({owner:contact.owner,recipient:contact.recipient})}`);
    if(request!==state.chatRequest)return;
    state.messages=data.messages;state.timeline=data.timeline;state.reference=data.prompt_reference;renderTimeline();error('');
    $('copy-prompt').disabled=false;$('prompt-request').disabled=false;
    $('timeline').scrollTop=keepScroll?oldScroll:$('timeline').scrollHeight;
    if(!keepScroll)showSearchMatch(contact);
  }catch(e){if(request===state.chatRequest){error(e.message);$('timeline').replaceChildren(el('div','empty-list','메일 로드 실패 · 새로고침하세요.'));}}
}
function renderTimeline(){
  const timeline=$('timeline');timeline.replaceChildren();let previousDay='',previousTopic='';
  state.timeline.forEach((item,index)=>{
    const day=fullDate(item.sent_at);
    if(day!==previousDay){const divider=el('div','date-divider');divider.append(el('span','',day));timeline.append(divider);previousDay=day;}
    if(item.subject&&item.subject!==previousTopic){timeline.append(el('div','topic-divider',item.subject));previousTopic=item.subject;}
    const row=el('article',`message-row${item.mine?' mine':''}`),bubble=el('div','message');
    row.dataset.messageIndex=index;
    if(!item.mine&&item.sender!==state.current.recipient)bubble.append(el('strong','message-sender',item.sender_name));
    const body=el('p','message-body',item.body);body.id=`message-body-${index}`;
    const toggle=button('더 보기','text-button message-toggle',()=>{
      const expanded=body.classList.toggle('expanded');
      toggle.textContent=expanded?'접기':'더 보기';toggle.setAttribute('aria-expanded',String(expanded));
      if(!expanded)row.scrollIntoView({block:'nearest'});
    });
    toggle.hidden=true;toggle.setAttribute('aria-expanded','false');toggle.setAttribute('aria-controls',body.id);
    bubble.append(body,toggle);
    const meta=el('div','message-meta');
    if(item.kind==='quote')meta.append(el('span','quote-source','인용'));
    meta.append(el('time','',clockTime(item.sent_at)));
    const mail=state.messages.find(mail=>mail.id===item.source_ids[0]);
    if(mail){
      const actions=[['원문 보기','document','original']];
      if(item.kind==='sent')actions.push(['문체 편집','edit','edit']);
      actions.forEach(([label,name,tab])=>{const action=button('','icon-button',()=>openEditor(mail,tab));action.setAttribute('aria-label',`${label}: ${mail.subject}`);action.title=label;action.append(icon(name));meta.append(action);});
    }
    const remove=button('','icon-button delete-button',()=>openDelete(item));
    remove.setAttribute('aria-label',`메시지 삭제: ${item.subject}`);remove.title='메시지 삭제';remove.append(icon('delete'));meta.append(remove);
    bubble.append(meta);row.append(bubble);timeline.append(row);
  });
  if(!state.timeline.length)timeline.append(el('div','empty-list','메일 없음'));
  updateMessageToggles();
}
function updateMessageToggles(){
  for(const body of $('timeline').querySelectorAll('.message-body')){
    const toggle=body.nextElementSibling;
    const limit=parseFloat(getComputedStyle(body).getPropertyValue('--message-preview-height'));
    toggle.hidden=body.scrollHeight<=limit+1;
    if(toggle.hidden){body.classList.remove('expanded');toggle.textContent='더 보기';toggle.setAttribute('aria-expanded','false');}
  }
}
function buildPrompt(){
  const {database_path,project_path,context_command}=state.reference;
  const request=$('prompt-request').value.trim()||'이 수신자에게 보낼 이메일 초안을 작성해 주세요. 작성 목적이 없으면 먼저 확인해 주세요.';
  return `프로젝트의 DB를 조회해 이메일 초안을 작성해 주세요.

프로젝트: [Mimicmail](<${project_path}>)
데이터베이스: [SQLite DB](<${database_path}>)
대화 식별자: owner=${state.current.owner}, recipient=${state.current.recipient}

작성 요청: ${request}

아래 읽기 전용 명령으로 해당 대화·직급·유사한 본인 발신 문체 예시를 조회하세요.
\`\`\`sh
${context_command}
\`\`\`
조회 결과의 instructions를 따르세요. current_facts.conversation과 위 작성 요청은 사실의 출처이며, style_references는 문체만 참고하세요. 삭제된 메시지는 제외하고, 확인되지 않은 사실은 만들지 마세요. 제목과 본문을 이 채팅에 작성하세요.`;
}
function openNewChat(){
  $('new-chat-form').reset();$('new-chat-error').hidden=true;
  const owners=state.data.owners;
  $('new-owner').value=state.current?.owner||owners[0]||'';
  $('new-owner-field').hidden=owners.length===1;
  $('known-owners').replaceChildren(...owners.map(owner=>new Option(owner,owner)));
  $('new-known-labels').replaceChildren(...state.data.labels.map(label=>new Option(label,label)));
  $('new-chat-dialog').showModal();$(owners.length?'new-recipient':'new-owner').focus();
}
async function createChat(){
  if(state.creating)return;
  state.creating=true;
  for(const input of $('new-chat-dialog').querySelectorAll('input,button'))input.disabled=true;
  $('new-chat-error').hidden=true;
  try{
    const result=await api('/api/conversation',{method:'POST',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({owner:$('new-owner').value,recipient:$('new-recipient').value,name:$('new-name').value,label:$('new-role').value})});
    $('new-chat-dialog').close();
    clearTimeout(searchTimer);state.query='';state.label='';state.replies=false;$('search').value='';
    $('filter-all').classList.add('active');$('filter-all').setAttribute('aria-pressed','true');
    $('filter-replies').classList.remove('active');$('filter-replies').setAttribute('aria-pressed','false');
    await loadConversations();await openConversation(result.contact);$('prompt-request').focus();
    if(!result.created)toast('기존 대화 열림');
  }catch(e){$('new-chat-error').textContent=e.message;$('new-chat-error').hidden=false;}
  finally{state.creating=false;for(const input of $('new-chat-dialog').querySelectorAll('input,button'))input.disabled=false;}
}
function openDelete(item){
  state.deleting={item,contact:state.current,revision:state.messages.find(mail=>mail.id===item.source_ids[0])?.revision};
  $('delete-subject').textContent=item.subject||'(제목 없음)';
  $('delete-sender').textContent=item.sender_name&&item.sender&&item.sender_name!==item.sender?`${item.sender_name} <${item.sender}>`:item.sender||item.sender_name||'작성자 미확인';
  $('delete-date').textContent=`${fullDate(item.sent_at)} ${clockTime(item.sent_at)}`.trim();
  $('delete-preview').textContent=item.body.length>160?item.body.slice(0,160)+'…':item.body;
  $('delete-error').hidden=true;$('delete-dialog').showModal();$('cancel-delete').focus();
}
function closeDelete(){if(!state.deleteSaving){$('delete-dialog').close();state.deleting=null;}}
async function deleteMessage(){
  if(!state.deleting||state.deleteSaving)return;
  const {item,contact,revision}=state.deleting;state.deleteSaving=true;
  $('confirm-delete').disabled=true;$('cancel-delete').disabled=true;$('delete-error').hidden=true;
  try{
    await api('/api/message',{method:'DELETE',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({owner:contact.owner,recipient:contact.recipient,key:item.keys[0],revision})});
    $('delete-dialog').close();state.deleting=null;
    await loadConversations({refreshChat:true});
    if(!state.timeline.length&&!state.current.registered){state.current=null;state.messages=[];renderContacts();$('contact-name').textContent='대화';$('contact-email').textContent='';$('contact-email').hidden=true;$('contact-avatar').textContent='m';$('role-tag').textContent='직급 지정';$('role-tag').disabled=true;$('reply-note').textContent='';$('prompt-request').value='';$('prompt-request').disabled=true;$('copy-prompt').disabled=true;state.reference=null;}
    toast('메시지 삭제됨');
  }catch(e){$('delete-error').textContent=e.message;$('delete-error').hidden=false;}
  finally{state.deleteSaving=false;$('confirm-delete').disabled=false;$('cancel-delete').disabled=false;}
}
function openLabel(){
  if(!state.current)return;
  $('role-label').value=state.current.label||'';$('label-error').hidden=true;
  $('known-labels').replaceChildren(...state.data.labels.map(label=>new Option(label,label)));
  $('label-editor').showModal();$('role-label').focus();
}
async function saveLabel(){
  if(state.labelSaving)return;
  const contact=state.current;state.labelSaving=true;
  for(const node of $('label-editor').querySelectorAll('input,button'))node.disabled=true;
  try{
    const result=await api('/api/contact-label',{method:'PUT',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({owner:contact.owner,recipient:contact.recipient,label:$('role-label').value,revision:contact.label_revision})});
    contact.label=result.label;contact.label_revision=result.revision;
    if(state.label)state.label=result.label;
    $('label-editor').close();
    await loadConversations({refreshChat:true});
  }catch(e){$('label-error').textContent=e.message;$('label-error').hidden=false;}
  finally{state.labelSaving=false;for(const node of $('label-editor').querySelectorAll('input,button'))node.disabled=false;}
}
function setTab(tab){
  if(tab==='edit'&&state.editing?.deleted)return;
  ['edit','original'].forEach(name=>{const selected=tab===name;$(`tab-${name}`).setAttribute('aria-selected',String(selected));$(`tab-${name}`).tabIndex=selected?0:-1;$(`panel-${name}`).hidden=!selected;});
}
function profileField(label,name,value,options){
  const wrap=el('label','field',label);
  const input=el('select');input.dataset.field=name;
  Object.entries(options).forEach(([key,text])=>{const option=el('option','',text);option.value=key;input.append(option);});
  input.value=value;input.addEventListener('input',markDirty);wrap.append(input);return wrap;
}
function openEditor(mail,tab){
  state.editing=mail;state.dirty=false;state.saving=false;$('tab-edit').disabled=mail.deleted;
  $('editor-subject').textContent=mail.subject||'(제목 없음)';$('editor-date').textContent=`${fullDate(mail.sent_at)} ${clockTime(mail.sent_at)}`;
  $('edit-body').value=mail.display_body;$('editor-error').hidden=true;$('save').disabled=true;$('save').textContent='변경 저장';
  $('edit-status').textContent='';
  $('original-body').textContent=mail.display_original;
  $('source-name').textContent=`원본 파일 · ${mail.source_name}`;
  const forms=$('recipient-fields');forms.replaceChildren();
  mail.recipients.forEach(profile=>{const form=el('section','recipient-form');form.dataset.recipient=profile.recipient;form.append(el('h4','',mail.context.recipient_names?.[profile.recipient]||profile.recipient.split('@')[0]));const fields=el('div','field-grid');fields.append(profileField('메일 목적','purpose',profile.purpose,state.data.purposes),profileField('나와의 관계','relationship',profile.relationship,state.data.relationships));form.append(fields);forms.append(form);});
  setTab(tab);$('editor').showModal();if(tab==='edit')$('edit-body').focus();
}
function markDirty(){state.dirty=true;$('save').disabled=state.saving;$('edit-status').textContent='저장 안 됨';}
function setSaving(value){state.saving=value;for(const input of $('editor').querySelectorAll('textarea,select,input,#restore-body'))input.disabled=value;$('save').disabled=value||!state.dirty;$('save').textContent=value?'저장 중…':'변경 저장';}
function closeEditor(){if(state.saving)return;if(state.dirty&&!confirm('변경 사항을 버리고 닫을까요?'))return;state.dirty=false;$('editor').close();}
async function save(){
  if(!state.dirty||state.saving)return;
  const body=$('edit-body').value;if(!body.trim()){showEditorError('문체 참조 본문을 입력하세요.');return;}
  const recipients=[...$('recipient-fields').children].map(form=>({recipient:form.dataset.recipient,role:state.editing.recipients.find(profile=>profile.recipient===form.dataset.recipient).role,...Object.fromEntries([...form.querySelectorAll('[data-field]')].map(input=>[input.dataset.field,input.value]))}));
  setSaving(true);$('editor-error').hidden=true;
  try{
    const updated=await api(`/api/mail/${state.editing.id}`,{method:'PUT',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({revision:state.editing.revision,authored_body:body,recipients})});
    state.editing=updated;state.dirty=false;$('edit-status').textContent='저장됨';
    await loadConversations({refreshChat:true});
  }catch(e){showEditorError(e.message);}finally{setSaving(false);}
}
function showEditorError(message){$('editor-error').textContent=message;$('editor-error').hidden=false;}
let searchTimer;
$('search').addEventListener('input',()=>{state.query=$('search').value;clearSearchMatch();clearTimeout(searchTimer);++state.listRequest;searchTimer=setTimeout(()=>loadConversations(),180);});
$('prompt-request').addEventListener('input',()=>{if(!state.current)return;const key=contactKey(state.current),value=$('prompt-request').value;if(value)requests.set(key,value);else requests.delete(key);});
for(const [id,replies] of [['filter-all',false],['filter-replies',true]]){$(id).addEventListener('click',()=>{state.replies=replies;for(const [other,value] of [['filter-all',false],['filter-replies',true]]){$(other).classList.toggle('active',replies===value);$(other).setAttribute('aria-pressed',String(replies===value));}loadConversations();});}
$('refresh').addEventListener('click',()=>loadConversations({refreshChat:true}));
$('back').addEventListener('click',()=>document.querySelector('.app').classList.remove('show-chat'));
$('filter-label').addEventListener('change',()=>{state.label=$('filter-label').value;loadConversations();});
$('role-tag').addEventListener('click',openLabel);
$('new-chat').addEventListener('click',openNewChat);
$('new-chat-form').addEventListener('submit',event=>{event.preventDefault();createChat();});
$('cancel-new-chat').addEventListener('click',()=>{if(!state.creating)$('new-chat-dialog').close();});
$('new-chat-dialog').addEventListener('cancel',event=>{if(state.creating)event.preventDefault();});
$('cancel-delete').addEventListener('click',closeDelete);
$('confirm-delete').addEventListener('click',deleteMessage);
$('delete-dialog').addEventListener('cancel',event=>{event.preventDefault();closeDelete();});
$('label-form').addEventListener('submit',event=>{event.preventDefault();saveLabel();});
$('close-label').addEventListener('click',()=>{if(!state.labelSaving)$('label-editor').close();});
$('label-editor').addEventListener('cancel',event=>{if(state.labelSaving)event.preventDefault();});
$('clear-label').addEventListener('click',()=>{$('role-label').value='';saveLabel();});
$('copy-prompt').addEventListener('click',async()=>{try{await navigator.clipboard.writeText(buildPrompt());toast('프롬프트 복사됨');}catch{error('복사 실패 · 브라우저의 클립보드 권한을 확인하세요.');}});
$('edit-body').addEventListener('input',markDirty);$('close-editor').addEventListener('click',closeEditor);$('save').addEventListener('click',save);
$('editor').addEventListener('cancel',event=>{event.preventDefault();closeEditor();});
$('restore-body').addEventListener('click',()=>{if(!state.editing)return;$('edit-body').value=state.editing.display_original_authored;markDirty();});
$('copy-body').addEventListener('click',async()=>{try{await navigator.clipboard.writeText($('panel-original').hidden?$('edit-body').value:state.editing.display_original);toast('본문 복사됨');}catch{showEditorError('복사 실패 · 본문을 선택해 직접 복사하세요.');}});
for(const tab of ['edit','original']){$(`tab-${tab}`).addEventListener('click',()=>setTab(tab));$(`tab-${tab}`).addEventListener('keydown',event=>{if(['ArrowLeft','ArrowRight','Home','End'].includes(event.key)){event.preventDefault();const next=event.key==='Home'?'edit':event.key==='End'?'original':tab==='edit'?'original':'edit';setTab(next);$(`tab-${next}`).focus();}});}
document.addEventListener('keydown',event=>{if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='k'&&!$('editor').open&&!$('label-editor').open&&!$('delete-dialog').open&&!$('new-chat-dialog').open){event.preventDefault();$('search').focus();}if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='s'&&$('editor').open){event.preventDefault();save();}});
window.addEventListener('beforeunload',event=>{if(state.dirty||[...requests.values()].some(value=>value.trim())){event.preventDefault();event.returnValue='';}});
$('conversations').replaceChildren(...Array.from({length:5},()=>el('div','loading-line')));
const messageResizeObserver=new ResizeObserver(updateMessageToggles);
messageResizeObserver.observe($('timeline'));
loadConversations();
