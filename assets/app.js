import {labelHue,labelHex,labelCss} from './label-colors.mjs';
import {api} from './api.mjs';

const $ = id => document.getElementById(id);
const state = {data:null, current:null, messages:[], timeline:[], reference:null, creating:false, query:'', label:'', unlabeled:false, matches:[], matchIndex:-1, matchTotal:0, page:null, chatPending:false, loadedQuery:'', listPending:false, labelSaving:false, deleting:null, deleteSaving:false, listRequest:0, chatRequest:0, editing:null, dirty:false, saving:false};
const requests = new Map();
const replies = new Map();
const sentMails = new Map();
let composerMode='request',messageSaving=false;
let replyEdit=null,replyEditSaving=false,renameContact=null,renameSaving=false;
let settingsData=null,settingsSaving=false,settingsController;
let conversationMenu=null,chatToDelete=null,chatDeleting=false;
let listController, chatController, editorController;
const icon = name => { const svg=document.createElementNS('http://www.w3.org/2000/svg','svg'); const use=document.createElementNS(svg.namespaceURI,'use'); use.setAttribute('href',`#${name}-icon`);svg.append(use);svg.setAttribute('aria-hidden','true');return svg; };
function el(tag, cls, text){const node=document.createElement(tag);if(cls)node.className=cls;if(text!==undefined)node.textContent=text;return node;}
function button(label,cls,fn){const b=el('button',cls,label);b.type='button';b.addEventListener('click',fn);return b;}
function initials(name){return name.replace(/[<>"\s]/g,'').slice(0,2);}
const dateFormat=new Intl.DateTimeFormat('ko-KR',{timeZone:'Asia/Seoul',year:'numeric',month:'long',day:'numeric',weekday:'short'});
const timeFormat=new Intl.DateTimeFormat('ko-KR',{timeZone:'Asia/Seoul',hour:'2-digit',minute:'2-digit',hour12:false});
function fullDate(date){return date ? dateFormat.format(new Date(date)) : '날짜 미상';}
function clockTime(date){return date ? timeFormat.format(new Date(date)) : '';}
function avatar(name){return el('div','avatar',initials(name));}
function applyLabelColor(node,color){
  const hue=labelHue(color);
  node.classList.toggle('colored',hue!==null);
  if(hue!==null)node.style.setProperty('--label-color',labelCss(hue));else node.style.removeProperty('--label-color');
}
function sameContact(a,b){return a&&b&&a.owner===b.owner&&a.recipient===b.recipient;}
function contactKey(contact){return JSON.stringify([contact.owner,contact.recipient]);}
function composerDrafts(){return composerMode==='sent'?sentMails:composerMode==='reply'?replies:requests;}
function syncComposer(){
  const disabled=!state.reference||state.listPending||state.loadedQuery!==state.query||messageSaving;
  $('prompt-request').disabled=$('copy-prompt').disabled=disabled;
  $('mode-request').disabled=$('mode-reply').disabled=$('mode-sent').disabled=disabled;
  $('save-message').disabled=disabled||!$('prompt-request').value.trim();
  $('copy-prompt').hidden=composerMode!=='request';$('save-message').hidden=composerMode==='request';
  $('save-message').textContent=messageSaving?'저장 중…':composerMode==='sent'?'메일 저장':'답장 저장';
  const label=composerMode==='sent'?'내가 보낸 메일 내용':composerMode==='reply'?'상대 답장 내용':'메일에 담을 내용';
  $('prompt-request').placeholder=label;$('prompt-request').setAttribute('aria-label',label);
}
function highlight(node,term){
  const text=node.textContent,index=text.indexOf(term);
  if(index<0)return null;
  const mark=el('mark','search-match',text.slice(index,index+term.length));
  node.replaceChildren(document.createTextNode(text.slice(0,index)),mark,document.createTextNode(text.slice(index+term.length)));
  return mark;
}
function clearSearchMatch(){
  for(const mark of $('timeline').querySelectorAll('mark.search-match')){const parent=mark.parentNode;mark.replaceWith(document.createTextNode(mark.textContent));parent.normalize();}
  state.matches=[];state.matchIndex=-1;state.matchTotal=0;$('search-navigation').hidden=true;
}
async function moveSearchMatch(step){
  if(!state.matchTotal||state.chatPending)return;
  const index=(state.matchIndex+step+state.matchTotal)%state.matchTotal;
  const mark=state.matches.find(mark=>Number(mark.dataset.matchIndex)===index);
  if(!mark){await openConversation(state.current,false,false,{match:index,revision:state.page.revision});return;}
  for(const previous of state.matches)previous.classList.remove('current-match');
  state.matchIndex=index;mark.classList.add('current-match');
  const body=mark.closest('.message-body');
  if(body){body.classList.add('expanded');const toggle=body.nextElementSibling;toggle.textContent='접기';toggle.setAttribute('aria-expanded','true');}
  $('search-position').textContent=`${index+1} / ${state.matchTotal}`;
  mark.scrollIntoView({block:'center'});
}
function showSearchMatches(matches,total,index){
  clearSearchMatch();
  if(!state.query.trim())return;
  const targets=new Map();
  for(const match of matches){
    const index=state.timeline.findIndex(item=>item.keys.includes(match.key));
    const node=match.field==='body'?$('timeline').querySelector(`[data-message-index="${index}"] .message-body`):$('timeline').querySelector(`[data-topic-index="${index}"]`);
    if(node){if(!targets.has(node))targets.set(node,[]);targets.get(node).push(match);}
  }
  for(const [node,ranges] of targets){
    const text=node.textContent,parts=[];let offset=0;
    for(const range of ranges){const mark=el('mark','search-match',text.slice(range.start,range.end));mark.dataset.matchIndex=range.index;parts.push(document.createTextNode(text.slice(offset,range.start)),mark);offset=range.end;}
    parts.push(document.createTextNode(text.slice(offset)));node.replaceChildren(...parts);
  }
  state.matches=[...$('timeline').querySelectorAll('mark.search-match')];
  state.matchTotal=total;state.matchIndex=index??-1;
  $('search-navigation').hidden=false;$('search-position').textContent=`${index===null?0:index+1} / ${total}`;
  $('previous-match').disabled=$('next-match').disabled=!total;
  if(index!==null)moveSearchMatch(0);
}
function clearConversation(){
  ++state.chatRequest;chatController?.abort();editorController?.abort();
  state.current=null;state.reference=null;state.page=null;state.chatPending=false;state.messages=[];state.timeline=[];clearSearchMatch();
  $('contact-name').textContent='대화';
  $('rename-contact').hidden=true;
  $('contact-avatar').textContent='m';$('role-tag').textContent='직급 지정';$('role-tag').disabled=true;
  $('timeline').replaceChildren();$('prompt-request').value='';
  syncComposer();applyLabelColor(document.querySelector('.chat'),'');
  document.querySelector('.app').classList.remove('show-chat');
}
function error(message){$('chat-error').textContent=message;$('chat-error').hidden=!message;}
let toastTimer;
function toast(message){$('toast').textContent=message;$('toast').hidden=false;clearTimeout(toastTimer);toastTimer=setTimeout(()=>$('toast').hidden=true,3500);}

async function loadConversations({refreshChat=false,reveal=null}={}){
  closeConversationMenu();
  const request=++state.listRequest;
  listController?.abort();listController=new AbortController();
  state.listPending=true;$('conversations').inert=true;syncComposer();
  $('list-status').textContent='불러오는 중…';
  try{
    const data=await api(`/api/conversations?${new URLSearchParams({q:state.query,label:state.label,unlabeled:state.unlabeled?'1':'0'})}`,{signal:listController.signal});
    if(request!==state.listRequest)return;
    state.data=data;$('new-chat').disabled=$('settings').disabled=false;
    const selected=data.items.find(item=>sameContact(item,state.current));
    if(!selected&&state.query&&sameContact(reveal?.contact,state.current)){
      clearTimeout(searchTimer);state.query='';$('search').value='';
      return await loadConversations({refreshChat,reveal});
    }
    if(selected)state.current=selected;else clearConversation();
    $('filter-label').replaceChildren(new Option('모든 직급',''),new Option('미지정','unlabeled'),...data.labels.map(label=>new Option(label,`label:${label}`)));
    $('filter-label').value=state.unlabeled?'unlabeled':state.label?`label:${state.label}`:'';
    $('list-status').textContent=state.query?`검색 결과 ${data.items.length}개`:'';
    renderContacts();error('');
    if(state.current){
      applyLabelColor(document.querySelector('.chat'),state.current.label_color);
      $('role-tag').textContent=state.current.label||'직급 지정';
      const target=sameContact(reveal?.contact,state.current)&&reveal.key?{key:reveal.key}:null;
      if(refreshChat||state.loadedQuery!==state.query||!state.reference)await openConversation(state.current,!target&&state.loadedQuery===state.query,false,target);
    }
  }catch(e){if(e.name!=='AbortError'&&request===state.listRequest){clearConversation();$('list-status').textContent='목록 로드 실패 · 페이지를 새로고침하세요.';error(e.message);}}
  finally{if(request===state.listRequest){state.listPending=false;$('conversations').inert=false;syncComposer();}}
}
function renderContacts(){
  closeConversationMenu();
  const list=$('conversations');list.replaceChildren();
  state.data.items.forEach(item=>{
    const wrapper=el('div','conversation-item');
    const row=button('',`conversation-row${sameContact(item,state.current)?' selected':''}`,()=>openConversation(item));
    row.setAttribute('aria-label',`${item.name}${item.label?`, ${item.label}`:''}`);
    row.setAttribute('aria-current',sameContact(item,state.current)?'true':'false');
    applyLabelColor(row,item.label_color);row.append(avatar(item.name));
    const content=el('div'),top=el('div','row-top');top.append(el('span','row-name',item.name));
    top.firstChild.title=item.name;
    const preview=el('p','row-subject',item.search_match?.preview||item.preview||item.subject||(item.registered?'새 대화':'(본문 없음)'));
    if(item.search_match)highlight(preview,item.search_match.term);
    if(item.label){const label=el('span','rank-chip',item.label);label.title=item.label;top.append(label);}
    content.append(top,preview);
    if(item.reply_count){const bottom=el('div','row-bottom'),badge=el('span','reply-badge');badge.append(icon('reply'),document.createTextNode('답장·인용'));bottom.append(badge);content.append(bottom);}
    const menu=button('⋯','icon-button conversation-menu-button',event=>openConversationMenu(item,menu,event));
    menu.setAttribute('aria-label',`${item.name} 대화 메뉴`);menu.setAttribute('aria-haspopup','menu');menu.setAttribute('aria-controls','conversation-menu');
    wrapper.addEventListener('contextmenu',event=>{event.preventDefault();openConversationMenu(item,menu,event);});
    wrapper.addEventListener('keydown',event=>{if(event.key==='ContextMenu'||event.shiftKey&&event.key==='F10'){event.preventDefault();openConversationMenu(item,menu,event);}});
    row.append(content);wrapper.append(row,menu);list.append(wrapper);
  });
  if(!state.data.items.length)list.append(el('div','empty-list',state.query||state.label||state.unlabeled?'검색 결과 없음':'메일 없음'));
}
function closeConversationMenu(restoreFocus=false){
  if(!conversationMenu)return;
  const {anchor}=conversationMenu;conversationMenu=null;
  $('conversation-menu').hidden=true;anchor.closest('.conversation-item')?.classList.remove('menu-open');
  if(restoreFocus&&anchor.isConnected)anchor.focus({preventScroll:true});
}
function openConversationMenu(contact,anchor,event){
  if(state.listPending)return;
  closeConversationMenu();conversationMenu={contact,anchor};
  const menu=$('conversation-menu'),bounds=anchor.getBoundingClientRect();
  menu.hidden=false;menu.setAttribute('aria-label',`${contact.name} 대화 메뉴`);anchor.closest('.conversation-item').classList.add('menu-open');
  const pointer=event.type==='contextmenu'&&(event.clientX||event.clientY);
  menu.style.left=`${Math.max(8,Math.min(pointer?event.clientX:bounds.right-menu.offsetWidth,innerWidth-menu.offsetWidth-8))}px`;
  menu.style.top=`${Math.max(8,Math.min(pointer?event.clientY:bounds.bottom,innerHeight-menu.offsetHeight-8))}px`;
  $('delete-chat-menu').focus({preventScroll:true});
}
function openDeleteChat(){
  if(!conversationMenu)return;
  const {contact}=conversationMenu;closeConversationMenu(true);chatToDelete=contact;
  $('delete-chat-name').textContent=contact.name;$('delete-chat-error').hidden=true;
  $('delete-chat-dialog').showModal();$('cancel-delete-chat').focus();
}
function closeDeleteChat(){if(!chatDeleting){$('delete-chat-dialog').close();chatToDelete=null;}}
async function deleteChat(){
  if(!chatToDelete||chatDeleting)return;
  const contact=chatToDelete;chatDeleting=true;
  $('confirm-delete-chat').disabled=$('cancel-delete-chat').disabled=true;$('delete-chat-error').hidden=true;
  try{
    await api('/api/conversation',{method:'DELETE',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({owner:contact.owner,recipient:contact.recipient})});
    $('delete-chat-dialog').close();chatToDelete=null;
    for(const drafts of [requests,replies,sentMails])drafts.delete(contactKey(contact));
    if(sameContact(contact,state.current))clearConversation();
    await loadConversations();
    (document.querySelector('.conversation-row.selected')||$('new-chat')).focus({preventScroll:true});toast('대화 삭제됨');
  }catch(e){$('delete-chat-error').textContent=e.message;$('delete-chat-error').hidden=false;}
  finally{chatDeleting=false;$('confirm-delete-chat').disabled=$('cancel-delete-chat').disabled=false;}
}
async function openConversation(contact,keepScroll=false,showChat=true,page=null){
  closeConversationMenu();
  if(showChat)document.querySelector('.app').classList.add('show-chat');
  if(!page&&sameContact(contact,state.current)&&state.reference&&!keepScroll&&state.loadedQuery===state.query){if(state.matchTotal)moveSearchMatch(0);$('timeline').focus({preventScroll:true});return;}
  const request=++state.chatRequest,query=state.query;const oldScroll=$('timeline').scrollTop;
  const previousPage=keepScroll&&sameContact(contact,state.current)&&state.loadedQuery===query?state.page:null;
  state.chatPending=true;
  chatController?.abort();chatController=new AbortController();editorController?.abort();
  state.messages=[];state.timeline=[];
  state.current=contact;renderContacts();clearSearchMatch();applyLabelColor(document.querySelector('.chat'),contact.label_color);
  $('contact-name').textContent=contact.name;
  $('rename-contact').hidden=!contact.registered;
  $('contact-avatar').textContent=initials(contact.name);
  $('role-tag').textContent=contact.label||'직급 지정';$('role-tag').disabled=false;
  state.reference=null;
  $('prompt-request').value=composerDrafts().get(contactKey(contact))||'';syncComposer();
  if(!keepScroll){$('timeline').replaceChildren(...Array.from({length:3},()=>el('div','loading-line')));}
  try{
    const data=await api(`/api/conversation?${new URLSearchParams({owner:contact.owner,recipient:contact.recipient,compact:'1',limit:'50',q:query,...(previousPage?{before:previousPage.end}:{}),...page})}`,{signal:chatController.signal});
    if(request!==state.chatRequest||query!==state.query)return;
    state.messages=data.messages;state.timeline=data.timeline;state.page=data.page;state.reference=data.prompt_reference;state.loadedQuery=query;state.chatPending=false;renderTimeline(data.search_matches);error('');
    syncComposer();
    $('timeline').scrollTop=keepScroll?oldScroll:$('timeline').scrollHeight;
    showSearchMatches(data.search_matches,data.match_total,data.match_index);
    if(page?.key){const index=state.timeline.findIndex(item=>item.keys.includes(page.key));$('timeline').querySelector(`[data-message-index="${index}"]`)?.scrollIntoView({block:'center'});}
    if(showChat&&!keepScroll&&document.activeElement===document.body)$('timeline').focus({preventScroll:true});
  }catch(e){if(e.name!=='AbortError'&&request===state.chatRequest){if(e.status===409&&page?.revision!==undefined){await loadConversations({refreshChat:true});return;}error(e.message);$('timeline').replaceChildren(el('div','empty-list','메일 로드 실패 · 페이지를 새로고침하세요.'));}}
  finally{if(request===state.chatRequest)state.chatPending=false;}
}
function renderTimeline(matches=[]){
  const timeline=document.createDocumentFragment();let previousDay='',previousTopic='';
  const page=state.page;
  if(page?.start>0)timeline.append(button('이전 메시지','text-button timeline-page',()=>openConversation(state.current,false,false,{before:page.start,revision:page.revision})));
  state.timeline.forEach((item,index)=>{
    const day=item.sent_at===undefined?'':fullDate(item.sent_at);
    if(day&&day!==previousDay){const divider=el('div','date-divider');divider.append(el('span','',day));timeline.append(divider);previousDay=day;}
    if(item.subject&&(item.subject!==previousTopic||matches.some(match=>match.field==='subject'&&item.keys.includes(match.key)))){const topic=el('div','topic-divider',item.subject);topic.dataset.topicIndex=index;timeline.append(topic);previousTopic=item.subject;}
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
    if(item.sent_at)meta.append(el('time','',clockTime(item.sent_at)));
    const mail=state.messages.find(mail=>mail.id===item.source_ids[0]);
    if(mail){
      const actions=[['원문 보기','document','original']];
      if(item.kind==='sent')actions.push(['문체 편집','edit','edit']);
      actions.forEach(([label,name,tab])=>{const action=button('','icon-button',()=>openEditor(mail,tab));action.setAttribute('aria-label',`${label}: ${mail.subject}`);action.title=label;action.append(icon(name));meta.append(action);});
    }
    if(item.kind==='received'){
      const edit=button('','icon-button',()=>openReplyEditor(item));edit.setAttribute('aria-label','답장 수정');edit.append(icon('edit'));meta.append(edit);
    }
    const remove=button('','icon-button delete-button',()=>openDelete(item));
    remove.setAttribute('aria-label',`메시지 삭제: ${item.subject}`);remove.title='메시지 삭제';remove.append(icon('delete'));meta.append(remove);
    bubble.append(meta);row.append(bubble);timeline.append(row);
  });
  if(!state.timeline.length)timeline.append(el('div','empty-list','메일 없음'));
  if(page&&page.end<page.total)timeline.append(button('다음 메시지','text-button timeline-page',async()=>{await openConversation(state.current,false,false,{before:Math.min(page.end+50,page.total),revision:page.revision});$('timeline').scrollTop=0;}));
  $('timeline').replaceChildren(timeline);
  updateMessageToggles();
}
function updateMessageToggles(){
  const measurements=[...$('timeline').querySelectorAll('.message-body')].map(body=>({body,hidden:body.scrollHeight<=parseFloat(getComputedStyle(body).getPropertyValue('--message-preview-height'))+1}));
  for(const {body,hidden} of measurements){
    const toggle=body.nextElementSibling;
    if(toggle.hidden!==hidden)toggle.hidden=hidden;
    if(hidden&&body.classList.contains('expanded')){body.classList.remove('expanded');toggle.textContent='더 보기';toggle.setAttribute('aria-expanded','false');}
  }
}
async function saveMessage(){
  if($('save-message').disabled||composerMode==='request')return;
  const contact=state.current,key=contactKey(contact),body=$('prompt-request').value,mode=composerMode,drafts=composerDrafts();
  messageSaving=true;syncComposer();error('');
  try{
    const result=await api(mode==='sent'?'/api/sent':'/api/reply',{method:'POST',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({owner:contact.owner,recipient:contact.recipient,body})});
    drafts.delete(key);
    if(sameContact(contact,state.current)&&composerMode===mode)$('prompt-request').value='';
    await loadConversations({refreshChat:true,reveal:{contact,key:result.key}});
    toast('저장됨');
  }catch(e){error(e.message);}
  finally{messageSaving=false;syncComposer();if(sameContact(contact,state.current)&&!$('prompt-request').disabled&&!document.querySelector('dialog[open]'))$('prompt-request').focus({preventScroll:true});}
}
function openReplyEditor(item){
  replyEdit={item,contact:state.current};$('reply-edit-body').value=item.body;
  $('reply-edit-error').hidden=true;$('save-reply-edit').disabled=true;
  $('reply-editor').showModal();$('reply-edit-body').focus();
}
function replyEditDirty(){return replyEdit&&$('reply-edit-body').value!==replyEdit.item.body;}
function closeReplyEditor(){
  if(replyEditSaving||replyEditDirty()&&!confirm('변경 사항을 버리고 닫을까요?'))return;
  $('reply-editor').close();replyEdit=null;$('reply-edit-body').value='';
}
async function saveReplyEdit(){
  if(!replyEditDirty()||replyEditSaving)return;
  const {item,contact}=replyEdit;replyEditSaving=true;$('reply-edit-error').hidden=true;
  for(const node of $('reply-editor').querySelectorAll('textarea,button'))node.disabled=true;
  try{
    const result=await api('/api/reply',{method:'PUT',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({owner:contact.owner,recipient:contact.recipient,key:item.keys[0],body:$('reply-edit-body').value,original_body:item.body})});
    $('reply-editor').close();replyEdit=null;$('reply-edit-body').value='';
    await loadConversations({refreshChat:true,reveal:{contact,key:result.key}});toast('저장됨');
  }catch(e){$('reply-edit-error').textContent=e.message;$('reply-edit-error').hidden=false;}
  finally{replyEditSaving=false;for(const node of $('reply-editor').querySelectorAll('textarea,button'))node.disabled=false;}
}
function openRename(){
  if(!state.current?.registered)return;
  renameContact={...state.current};$('rename-name').value=renameContact.name;
  $('rename-error').hidden=true;$('save-name').disabled=true;
  $('rename-dialog').showModal();$('rename-name').focus();$('rename-name').select();
}
function renameDirty(){return renameContact&&$('rename-name').value!==renameContact.name;}
function closeRename(){
  if(renameSaving||renameDirty()&&!confirm('변경 사항을 버리고 닫을까요?'))return;
  $('rename-dialog').close();renameContact=null;
}
async function saveName(){
  if(!renameDirty()||renameSaving)return;
  const contact=renameContact;renameSaving=true;$('rename-error').hidden=true;
  for(const node of $('rename-dialog').querySelectorAll('input,button'))node.disabled=true;
  try{
    await api('/api/conversation',{method:'PUT',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({owner:contact.owner,recipient:contact.recipient,name:$('rename-name').value,original_name:contact.name})});
    $('rename-dialog').close();renameContact=null;
    await loadConversations({refreshChat:true,reveal:{contact}});toast('저장됨');
  }catch(e){$('rename-error').textContent=e.message;$('rename-error').hidden=false;}
  finally{renameSaving=false;for(const node of $('rename-dialog').querySelectorAll('input,button'))node.disabled=false;}
}
function buildPrompt(){
  const {database_path}=state.reference;
  const database=database_path==='data/mimicmail.sqlite3'?'':`\nDB: ${database_path}`;
  const request=$('prompt-request').value.trim()||'작성 목적을 먼저 확인해 주세요.';
  return `README의 context 조회 방법으로 필요한 대화와 내 문체를 참고해 이메일 제목·본문을 이 채팅에 작성해 주세요.
대화: ${state.current.name}${database}
owner=${state.current.owner}, recipient=${state.current.recipient}
요청: ${request}`;
}
function setupLabelChoice(id,value=''){
  const labels=state.data.labels,select=$(id+'-select');
  select.replaceChildren(new Option(labels.length?(select.required?'라벨 선택':'미지정'):'라벨 없음',''),...labels.map(label=>new Option(label,label)));
  select.value=value;$(id).value='';
  $(id+'-existing').checked=labels.length>0;$(id+'-new').checked=!labels.length;
  syncLabelChoice(id);
}
function syncLabelChoice(id){
  const existing=$(id+'-existing').checked;
  $(id+'-select').closest('.field').hidden=!existing;$(id+'-select').disabled=!existing;
  $(id).closest('.field').hidden=existing;$(id).disabled=existing;
}
function labelChoiceValue(id){return ($(id+'-existing').checked?$(id+'-select'):$(id)).value.trim();}
function contextEntries(){
  return [...$('context-entries').children].map(row=>({key:row.querySelector('input').value.trim(),value:row.querySelector('textarea').value.trim()})).filter(entry=>entry.key||entry.value);
}
function settingsDirty(){return settingsData!==null&&JSON.stringify(contextEntries())!==JSON.stringify(settingsData.entries);}
function syncSettings(){
  $('save-settings').disabled=settingsSaving||!settingsDirty();
  for(const row of $('context-entries').children){const key=row.querySelector('input'),value=row.querySelector('textarea');key.required=value.required=Boolean(key.value.trim()||value.value.trim());value.style.height='40px';value.style.height=`${Math.min(value.scrollHeight+2,160)}px`;}
}
function addContext(entry={key:'',value:''}){
  const row=el('div','context-row'),key=el('input'),value=el('textarea');
  key.type='text';key.maxLength=100;key.autocomplete='off';key.setAttribute('aria-label','키');key.value=entry.key;
  value.rows=1;value.maxLength=10000;value.setAttribute('aria-label','값');value.value=entry.value;
  const remove=button('','icon-button',()=>{const next=row.nextElementSibling||row.previousElementSibling;row.remove();syncSettings();(next?.querySelector('input')||$('add-context')).focus();});
  remove.setAttribute('aria-label','항목 삭제');remove.append(icon('delete'));
  row.append(key,value,remove);$('context-entries').append(row);return key;
}
async function openSettings(){
  settingsData=null;settingsController=new AbortController();
  $('context-entries').replaceChildren();$('settings-error').hidden=true;$('settings-status').hidden=false;
  $('add-context').disabled=$('save-settings').disabled=true;$('settings-dialog').showModal();
  const controller=settingsController;
  try{
    const data=await api('/api/general-context',{signal:controller.signal});
    if(controller.signal.aborted)return;
    settingsData=data;
    for(const entry of data.entries.length?data.entries:[{key:'',value:''}])addContext(entry);
    $('add-context').disabled=false;syncSettings();$('context-entries').querySelector('input').focus();
  }catch(e){if(e.name!=='AbortError'){$('settings-error').textContent=e.message;$('settings-error').hidden=false;}}
  finally{if(settingsController===controller)$('settings-status').hidden=true;}
}
function closeSettings(){
  if(settingsSaving||settingsDirty()&&!confirm('변경 사항을 버리시겠습니까?'))return;
  settingsController?.abort();settingsData=null;$('settings-dialog').close();
}
async function saveSettings(){
  if(settingsSaving||!settingsDirty()||!$('settings-form').reportValidity())return;
  const entries=contextEntries();
  if(new Set(entries.map(entry=>entry.key)).size!==entries.length){$('settings-error').textContent='중복된 키를 수정하거나 삭제하세요.';$('settings-error').hidden=false;return;}
  settingsSaving=true;$('settings-error').hidden=true;
  for(const control of $('settings-form').querySelectorAll('input,textarea,button'))control.disabled=true;
  try{
    await api('/api/general-context',{method:'PUT',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({entries,revision:settingsData.revision})});
    settingsData=null;$('settings-dialog').close();toast('저장됨');
  }catch(e){$('settings-error').textContent=e.message;$('settings-error').hidden=false;}
  finally{settingsSaving=false;for(const control of $('settings-form').querySelectorAll('input,textarea,button'))control.disabled=false;syncSettings();}
}
function openNewChat(){
  $('new-chat-form').reset();$('new-chat-error').hidden=true;
  setupLabelChoice('new-role');
  $('new-chat-dialog').showModal();$('new-name').focus();
}
async function createChat(){
  if(state.creating)return;
  state.creating=true;
  for(const input of $('new-chat-dialog').querySelectorAll('input,select,button'))input.disabled=true;
  $('new-chat-error').hidden=true;
  try{
    const result=await api('/api/conversation',{method:'POST',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({owner:state.current?.owner||state.data.owners[0]||'local',name:$('new-name').value,label:labelChoiceValue('new-role')})});
    $('new-chat-dialog').close();
    clearTimeout(searchTimer);state.query='';state.label='';state.unlabeled=false;$('search').value='';
    await loadConversations();await openConversation(result.contact);$('prompt-request').focus();
    if(!result.created)toast('기존 대화 열림');
  }catch(e){$('new-chat-error').textContent=e.message;$('new-chat-error').hidden=false;}
  finally{state.creating=false;for(const input of $('new-chat-dialog').querySelectorAll('input,select,button'))input.disabled=false;syncLabelChoice('new-role');}
}
function openDelete(item){
  state.deleting={item,contact:state.current,revision:state.messages.find(mail=>mail.id===item.source_ids[0])?.revision};
  $('delete-subject').textContent=item.subject||'(제목 없음)';
  $('delete-sender').textContent=item.sender_name||'작성자 미확인';
  $('delete-date').textContent=`${fullDate(item.sent_at)} ${clockTime(item.sent_at)}`.trim();
  $('delete-date').hidden=$('delete-date-label').hidden=item.sent_at===undefined;
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

    toast('메시지 삭제됨');
  }catch(e){$('delete-error').textContent=e.message;$('delete-error').hidden=false;}
  finally{state.deleteSaving=false;$('confirm-delete').disabled=false;$('cancel-delete').disabled=false;}
}
function openLabel(){
  if(!state.current)return;
  setupLabelChoice('role-label',state.current.label||'');$('label-error').hidden=true;
  syncLabelColor();$('label-editor').showModal();$($('role-label-existing').checked?'role-label-select':'role-label').focus();
}
function syncLabelColor(){
  const saved=state.data.label_colors[labelChoiceValue('role-label')];
  const hue=labelHue(saved?.color);
  $('label-color-value').value=hue===null?'':labelHex(hue);$('label-hue').value=hue??25;
  previewLabelColor();
}
function previewLabelColor(){
  const color=$('label-color-value').value,hue=labelHue(color);
  const name=hue===null?'색상 없음':hue<45?'빨강':hue<85?'주황':hue<125?'노랑':hue<175?'초록':hue<225?'청록':hue<275?'파랑':'보라';
  $('label-preview-avatar').textContent=initials(state.current.name);
  $('label-preview-name').textContent=state.current.name;
  $('label-preview-role').textContent=labelChoiceValue('role-label')||'미지정';
  $('label-hue').setAttribute('aria-valuetext',name);
  $('label-hue').style.setProperty('--label-color',hue===null?'#777':labelCss(hue));
  applyLabelColor($('label-color-preview'),color);
}
async function saveLabel(label=labelChoiceValue('role-label')){
  if(state.labelSaving)return;
  const contact=state.current;state.labelSaving=true;
  $('label-error').hidden=true;
  for(const node of $('label-editor').querySelectorAll('input,select,button'))node.disabled=true;
  try{
    const result=await api('/api/contact-label',{method:'PUT',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({owner:contact.owner,recipient:contact.recipient,label,revision:contact.label_revision,color:label?$('label-color-value').value.trim():'',color_revision:state.data.label_colors[label]?.revision||0})});
    contact.label=result.label;contact.label_revision=result.revision;
    $('label-editor').close();
    await loadConversations({refreshChat:true});
  }catch(e){$('label-error').textContent=e.message;$('label-error').hidden=false;}
  finally{state.labelSaving=false;for(const node of $('label-editor').querySelectorAll('input,select,button'))node.disabled=false;syncLabelChoice('role-label');}
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
async function openEditor(mail,tab){
  editorController?.abort();const controller=editorController=new AbortController();
  try{mail=await api(`/api/mail/${mail.id}`,{signal:controller.signal});}
  catch(e){if(e.name!=='AbortError')error(e.message);return;}
  if(controller.signal.aborted)return;
  state.editing=mail;state.dirty=false;state.saving=false;$('tab-edit').disabled=mail.deleted;
  $('editor-subject').textContent=mail.subject||'(제목 없음)';$('editor-date').textContent=`${fullDate(mail.sent_at)} ${clockTime(mail.sent_at)}`;
  $('editor-subject').hidden=!mail.subject;$('editor-date').hidden=mail.context.after_reply_id!==undefined;
  $('edit-body').value=mail.display_body;$('editor-error').hidden=true;$('save').disabled=true;$('save').textContent='변경 저장';
  $('edit-status').textContent='';
  $('original-body').textContent=mail.display_original;
  $('source-name').textContent=`원본 파일 · ${mail.source_name}`;
  $('source-name').hidden=!mail.source_name;
  const forms=$('recipient-fields');forms.replaceChildren();
  mail.recipients.forEach(profile=>{const form=el('section','recipient-form');form.dataset.recipient=profile.recipient;form.append(el('h4','',mail.context.recipient_names?.[profile.recipient]||'수신자'));const fields=el('div','field-grid');fields.append(profileField('메일 목적','purpose',profile.purpose,state.data.purposes),profileField('나와의 관계','relationship',profile.relationship,state.data.relationships));form.append(fields);forms.append(form);});
  setTab(tab);$('editor').showModal();if(tab==='edit')$('edit-body').focus();
}
function markDirty(){state.dirty=true;$('save').disabled=state.saving;$('edit-status').textContent='저장 안 됨';}
function setSaving(value){state.saving=value;for(const input of $('editor').querySelectorAll('textarea,select,input,#restore-body'))input.disabled=value;$('save').disabled=value||!state.dirty;$('save').textContent=value?'저장 중…':'변경 저장';}
function closeEditor(){if(state.saving)return;if(state.dirty&&!confirm('변경 사항을 버리고 닫을까요?'))return;state.dirty=false;state.editing=null;$('editor').close();$('edit-body').value='';$('original-body').textContent='';$('recipient-fields').replaceChildren();}
async function save(){
  if(!state.dirty||state.saving)return;
  const body=$('edit-body').value;if(!body.trim()){showEditorError('문체 참조 본문을 입력하세요.');return;}
  const recipients=[...$('recipient-fields').children].map(form=>({recipient:form.dataset.recipient,role:state.editing.recipients.find(profile=>profile.recipient===form.dataset.recipient).role,...Object.fromEntries([...form.querySelectorAll('[data-field]')].map(input=>[input.dataset.field,input.value]))}));
  setSaving(true);$('editor-error').hidden=true;
  try{
    const updated=await api(`/api/mail/${state.editing.id}`,{method:'PUT',headers:{'Content-Type':'application/json','X-Mimicmail-Token':state.data.token},body:JSON.stringify({revision:state.editing.revision,authored_body:body,recipients})});
    state.editing=updated;state.dirty=false;$('edit-body').value=updated.display_body;$('edit-status').textContent='저장됨';
    await loadConversations({refreshChat:true});
  }catch(e){showEditorError(e.message);}finally{setSaving(false);}
}
function showEditorError(message){$('editor-error').textContent=message;$('editor-error').hidden=false;}
let searchTimer;
$('search').addEventListener('input',()=>{state.query=$('search').value;syncComposer();clearSearchMatch();clearTimeout(searchTimer);++state.listRequest;listController?.abort();++state.chatRequest;chatController?.abort();state.chatPending=false;searchTimer=setTimeout(()=>loadConversations(),180);});
$('prompt-request').addEventListener('input',()=>{if(!state.current)return;const key=contactKey(state.current),value=$('prompt-request').value;if(value)composerDrafts().set(key,value);else composerDrafts().delete(key);syncComposer();});
for(const mode of ['request','sent','reply'])$(`mode-${mode}`).addEventListener('change',()=>{composerMode=mode;$('prompt-request').value=state.current?composerDrafts().get(contactKey(state.current))||'':'';syncComposer();});
$('save-message').addEventListener('click',saveMessage);
$('reply-edit-body').addEventListener('input',()=>{$('save-reply-edit').disabled=replyEditSaving||!replyEditDirty()||!$('reply-edit-body').value.trim();});
$('reply-edit-form').addEventListener('submit',event=>{event.preventDefault();saveReplyEdit();});
$('cancel-reply-edit').addEventListener('click',closeReplyEditor);
$('reply-editor').addEventListener('cancel',event=>{event.preventDefault();closeReplyEditor();});
$('rename-contact').addEventListener('click',openRename);
$('rename-name').addEventListener('input',()=>{$('save-name').disabled=renameSaving||!renameDirty()||!$('rename-name').value.trim();});
$('rename-form').addEventListener('submit',event=>{event.preventDefault();saveName();});
$('cancel-rename').addEventListener('click',closeRename);
$('rename-dialog').addEventListener('cancel',event=>{event.preventDefault();closeRename();});
$('back').addEventListener('click',()=>{document.querySelector('.app').classList.remove('show-chat');document.querySelector('.conversation-row.selected')?.focus({preventScroll:true});});
$('filter-label').addEventListener('change',()=>{const value=$('filter-label').value;state.unlabeled=value==='unlabeled';state.label=value.startsWith('label:')?value.slice(6):'';loadConversations();});
$('role-tag').addEventListener('click',openLabel);
$('settings').addEventListener('click',openSettings);
$('add-context').addEventListener('click',()=>{addContext().focus();syncSettings();});
$('context-entries').addEventListener('input',()=>{$('settings-error').hidden=true;syncSettings();});
$('settings-form').addEventListener('submit',event=>{event.preventDefault();saveSettings();});
$('cancel-settings').addEventListener('click',closeSettings);
$('settings-dialog').addEventListener('cancel',event=>{event.preventDefault();closeSettings();});
$('new-chat').addEventListener('click',openNewChat);
$('new-chat-form').addEventListener('submit',event=>{event.preventDefault();createChat();});
$('cancel-new-chat').addEventListener('click',()=>{if(!state.creating)$('new-chat-dialog').close();});
$('new-chat-dialog').addEventListener('cancel',event=>{if(state.creating)event.preventDefault();});
$('cancel-delete').addEventListener('click',closeDelete);
$('delete-chat-menu').addEventListener('click',openDeleteChat);
$('cancel-delete-chat').addEventListener('click',closeDeleteChat);
$('confirm-delete-chat').addEventListener('click',deleteChat);
$('delete-chat-dialog').addEventListener('cancel',event=>{event.preventDefault();closeDeleteChat();});
document.addEventListener('pointerdown',event=>{if(conversationMenu&&!$('conversation-menu').contains(event.target))closeConversationMenu();});
document.addEventListener('keydown',event=>{if(!conversationMenu)return;if(event.key==='Escape'){event.preventDefault();closeConversationMenu(true);}else if(event.key==='Tab'){closeConversationMenu(true);}else if(['ArrowDown','ArrowUp','Home','End'].includes(event.key)){event.preventDefault();$('delete-chat-menu').focus();}});
window.addEventListener('resize',()=>closeConversationMenu());
document.addEventListener('scroll',()=>closeConversationMenu(),true);
$('confirm-delete').addEventListener('click',deleteMessage);
$('delete-dialog').addEventListener('cancel',event=>{event.preventDefault();closeDelete();});
$('label-form').addEventListener('submit',event=>{event.preventDefault();saveLabel();});
$('close-label').addEventListener('click',()=>{if(!state.labelSaving)$('label-editor').close();});
$('label-editor').addEventListener('cancel',event=>{if(state.labelSaving)event.preventDefault();});
$('role-label').addEventListener('input',syncLabelColor);
for(const id of ['new-role','role-label']){
  for(const mode of ['existing','new'])$(id+'-'+mode).addEventListener('change',()=>{syncLabelChoice(id);if(id==='role-label')syncLabelColor();});
}
$('role-label-select').addEventListener('change',syncLabelColor);
$('label-hue').addEventListener('input',()=>{$('label-color-value').value=labelHex(Number($('label-hue').value));previewLabelColor();});
$('clear-color').addEventListener('click',()=>{$('label-color-value').value='';previewLabelColor();});
$('previous-match').addEventListener('click',()=>moveSearchMatch(-1));
$('next-match').addEventListener('click',()=>moveSearchMatch(1));
$('search').addEventListener('keydown',event=>{if(event.key==='Enter'){event.preventDefault();moveSearchMatch(event.shiftKey?-1:1);}});
$('clear-label').addEventListener('click',()=>saveLabel(''));
$('copy-prompt').addEventListener('click',async()=>{try{await navigator.clipboard.writeText(buildPrompt());toast('프롬프트 복사됨');}catch{error('복사 실패 · 브라우저의 클립보드 권한을 확인하세요.');}});
$('edit-body').addEventListener('input',markDirty);$('close-editor').addEventListener('click',closeEditor);$('save').addEventListener('click',save);
$('editor').addEventListener('cancel',event=>{event.preventDefault();closeEditor();});
$('restore-body').addEventListener('click',()=>{if(!state.editing)return;$('edit-body').value=state.editing.display_original_authored;markDirty();});
$('copy-body').addEventListener('click',async()=>{try{await navigator.clipboard.writeText($('panel-original').hidden?$('edit-body').value:state.editing.display_original);toast('본문 복사됨');}catch{showEditorError('복사 실패 · 본문을 선택해 직접 복사하세요.');}});
for(const tab of ['edit','original']){$(`tab-${tab}`).addEventListener('click',()=>setTab(tab));$(`tab-${tab}`).addEventListener('keydown',event=>{if(['ArrowLeft','ArrowRight','Home','End'].includes(event.key)){event.preventDefault();const next=event.key==='Home'?'edit':event.key==='End'?'original':tab==='edit'?'original':'edit';setTab(next);$(`tab-${next}`).focus();}});}
document.addEventListener('keydown',event=>{if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='k'&&!document.querySelector('dialog[open]')){event.preventDefault();if(window.matchMedia('(max-width:700px)').matches)document.querySelector('.app').classList.remove('show-chat');$('search').focus();}if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='s'){if($('editor').open){event.preventDefault();save();}else if($('reply-editor').open){event.preventDefault();$('reply-edit-form').requestSubmit();}else if($('rename-dialog').open){event.preventDefault();$('rename-form').requestSubmit();}}});
window.addEventListener('beforeunload',event=>{if(state.dirty||replyEditDirty()||renameDirty()||settingsDirty()||[...requests.values(),...replies.values(),...sentMails.values()].some(value=>value.trim())){event.preventDefault();event.returnValue='';}});
$('conversations').replaceChildren(...Array.from({length:5},()=>el('div','loading-line')));
const messageResizeObserver=new ResizeObserver(updateMessageToggles);
messageResizeObserver.observe($('timeline'));
loadConversations();
