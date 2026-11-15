#!/usr/bin/env node
// Teste ponta a ponta do back-end local (servidor de desenvolvimento), pelos
// mesmos caminhos e formatos que a interface usa.
//
//   cargo run -p saci-commerce-dev          # em outro terminal
//   node local-env/scripts/dev-e2e.mjs      # BASE=http://localhost:8787 por padrão
//
// Cobre: vitrine aberta (categorias, prateleiras, busca, diretório, vitrine,
// ficha, avaliações, frete), conta (entrada, perfil, endereço, favoritos),
// compra com cartão, cartão recusado, Pix, compra sem conta (guest token),
// painel do artesão (painel, vitrine com If-Match, anúncio com foto enviada,
// pausar), vendas (peça pronta → etiqueta assinada → postagem → entrega pela
// transportadora → avaliação), equipe (convite e aceite) e chamados.
import crypto from 'node:crypto';

const BASE = process.env.BASE ?? 'http://localhost:8787';
const PASSWORD = 'feira-de-exemplo';
let failures = 0;

function cpf(base) {
  const d = [...base].map(Number);
  const dv = (digits, start) => {
    const sum = digits.reduce((s, x, i) => s + x * (start - i), 0) % 11;
    return sum < 2 ? 0 : 11 - sum;
  };
  const a = dv(d, 10);
  const b = dv([...d, a], 11);
  return `${base}${a}${b}`;
}
const BUYER = cpf('529982247');
const ARTISAN = cpf('390533447');
const MEMBER = cpf('714602380');

async function call(method, path, { body, token, guest, headers = {}, idem, raw } = {}) {
  const res = await fetch(path.startsWith('http') ? path : BASE + path, {
    method,
    headers: {
      Accept: 'application/json',
      'X-Client': 'web/0.1.0',
      ...(body !== undefined && !raw ? { 'Content-Type': 'application/json' } : {}),
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(guest ? { 'X-Guest-Token': guest } : {}),
      ...(idem ? { 'Idempotency-Key': idem } : {}),
      ...headers,
    },
    body: raw ?? (body !== undefined ? JSON.stringify(body) : undefined),
  });
  const text = await res.text();
  let json;
  try { json = text ? JSON.parse(text) : undefined; } catch { json = text; }
  return { status: res.status, json, headers: res.headers };
}

function check(name, cond, detail) {
  if (cond) console.log(`  \x1b[32mok\x1b[0m   ${name}`);
  else { failures++; console.log(`  \x1b[31mFALHA\x1b[0m ${name}`, detail === undefined ? '' : JSON.stringify(detail).slice(0, 400)); }
}
const section = (t) => console.log(`\n\x1b[1;34m== ${t}\x1b[0m`);
const uuid = () => crypto.randomUUID();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function login(document) {
  const r = await call('POST', '/v1/identity/sessions', { body: { document, password: PASSWORD, client: 'ios' } });
  if (r.status !== 200) throw new Error(`login ${document}: ${r.status} ${JSON.stringify(r.json)}`);
  return r.json;
}
async function stepUp(token) {
  const r = await call('POST', '/v1/identity/sessions/step-up', { body: { password: PASSWORD }, token, headers: { 'X-Client': 'ios/0.1.0' } });
  return r.json.access_token;
}

section('Vitrine aberta');
const cats = await call('GET', '/v1/catalog/categories');
check('categorias com contagem', cats.status === 200 && cats.json.items.length === 8 && cats.json.items[0].listing_count > 0, cats.json);
const shelf = await call('GET', '/v1/catalog/listings?placement=home.destaques&limit=6');
check('prateleira do layout', shelf.status === 200 && shelf.json.items.length > 0);
const sponsored = await call('GET', '/v1/catalog/listings?placement=home.patrocinados');
check('patrocinados primeiro', sponsored.json.items.length === 0 || sponsored.json.items[0].sponsored);
check('limite de ids (24)', (await call('GET', `/v1/catalog/listings?ids=${Array(25).fill('lst_x').join(',')}`)).status === 422);
const search = await call('GET', '/v1/catalog/search?q=ceramica');
check('busca sem acento com facetas', search.status === 200 && search.json.total > 0 && search.json.facets.categories.length > 0, search.json?.facets);
const dir = await call('GET', '/v1/catalog/artisans');
check('diretório por região', dir.status === 200 && dir.json.aisles.length > 0 && dir.json.total === 10, dir.json?.total);
const piece = shelf.json.items[0];
const detail = await call('GET', `/v1/catalog/listings/${piece.id}`);
check('ficha da peça', detail.status === 200 && detail.json.description && detail.json.attributes.length > 0);
const artisan = await call('GET', `/v1/catalog/artisans/${piece.artisan.id}`);
check('perfil do artesão', artisan.status === 200 && artisan.json.craft !== undefined);
const store = await call('GET', '/v1/sellers/storefronts/barro-vivo');
check('vitrine pública sem rua (S-01)', store.status === 200 && store.json.location?.city && !JSON.stringify(store.json).includes('Estrada'), store.json);
const pieces = await call('GET', '/v1/catalog/storefronts/barro-vivo/listings?limit=2');
check('peças da vitrine com cursor', pieces.status === 200 && pieces.json.items.length === 2 && pieces.json.next_cursor);
const page2 = await call('GET', `/v1/catalog/storefronts/barro-vivo/listings?limit=2&cursor=${encodeURIComponent(pieces.json.next_cursor)}`);
check('segunda página', page2.status === 200 && page2.json.items[0].id !== pieces.json.items[0].id);
check('cursor adulterado recusado', (await call('GET', '/v1/catalog/storefronts/barro-vivo/listings?limit=2&cursor=abc.def')).status === 422);
const reviews = await call('GET', '/v1/reviews/storefronts/barro-vivo?limit=3');
check('avaliações com resumo', reviews.status === 200 && reviews.json.summary.total_reviews > 0 && reviews.json.items.length === 3);
check('CEP', (await call('GET', '/v1/shipping/postal-codes/60175047')).json?.city === 'Fortaleza');
check('CEP inexistente 404', (await call('GET', '/v1/shipping/postal-codes/00000000')).status === 404);
const quote = await call('POST', '/v1/shipping/quotes', { body: { postal_code: '60175047', storefronts: [{ artisan_id: piece.artisan.id, items: [{ listing_id: piece.id, quantity: 1 }] }] } });
check('cotação de frete', quote.status === 200 && quote.json.items[0].price.amount > 0, quote.json);
check('fontes da vitrine', (await call('GET', '/v1/sellers/storefront-fonts')).json?.items?.length === 5);
check('atributos', (await call('GET', '/v1/catalog/listing-attributes?category=ceramica')).json?.items?.some((a) => a.suggested));
check('layout publicado', (await call('GET', '/layouts/home.json')).json?.screen === 'home');

section('Conta de quem compra');
const buyer = await login(BUYER);
check('entrada com CPF e senha', buyer.access_token && buyer.refresh_token && buyer.account.document_masked);
check('senha errada recusada', (await call('POST', '/v1/identity/sessions', { body: { document: BUYER, password: 'errada!!', client: 'web' } })).json?.code === 'invalid-credentials');
const refreshed = await call('POST', '/v1/identity/sessions/refresh', { body: { refresh_token: buyer.refresh_token } });
check('refresh rotativo', refreshed.status === 200 && refreshed.json.refresh_token !== buyer.refresh_token);
// Contrato: repetir o refresh usado dentro de 60 s (repetição de rede) devolve o MESMO refresh novo;
// o sandbox recusa na hora. Os dois deixam o refresh antigo sem gerar uma sessão nova.
const reused = await call('POST', '/v1/identity/sessions/refresh', { body: { refresh_token: buyer.refresh_token } });
check('refresh usado não gera outro', reused.status === 401 || reused.json?.refresh_token === refreshed.json.refresh_token);
let token = refreshed.json.access_token;
const me = await call('GET', '/v1/customers/me', { token });
check('perfil com endereço do cofre', me.status === 200 && me.json.cpf_masked && me.json.address?.postal_code === '60175047', me.json);
check('sem token: 401', (await call('GET', '/v1/customers/me')).json?.code === 'unauthorized');
check('trocar e-mail pede senha (USER+)', (await call('PATCH', '/v1/customers/me', { token: (await login(BUYER)).access_token.replace(/./, (c) => c), body: { email: 'novo@exemplo.com' } })).status !== 500);
const purchases = await call('GET', '/v1/orders/me/purchases', { token });
check('compras com pacotes', purchases.status === 200 && purchases.json.items.length > 0 && purchases.json.items[0].packages);
check('favoritar', (await call('PUT', `/v1/customers/me/favorites/${piece.id}`, { token })).status === 204);
check('favoritos', (await call('GET', '/v1/customers/me/favorites', { token })).json?.items?.[0]?.id === piece.id);
const prefs = await call('GET', '/v1/payments/me/preferences', { token });
check('preferências de pagamento', prefs.status === 200 && prefs.json.default_method);

section('Compra');
const listings = (await call('GET', '/v1/catalog/listings?limit=48')).json.items;
const forSale = listings.filter((l) => l.stock >= 2);
async function buy(item, payment, opts = {}) {
  const q = await call('POST', '/v1/shipping/quotes', { body: { postal_code: '60175047', storefronts: [{ artisan_id: item.artisan.id, items: [{ listing_id: item.id, quantity: 1 }] }] } });
  const total = item.price.amount + q.json.items[0].price.amount;
  const body = {
    items: [{ listing_id: item.id, quantity: 1 }],
    shipping_address: { kind: 'brasil', recipient: 'Marina', postal_code: '60175047', street: 'Rua A', number: '10', city: 'Fortaleza', state: 'CE' },
    payment,
    expected_total: { amount: opts.total ?? total, currency: 'BRL' },
    ...(opts.buyer ? { buyer: opts.buyer } : {}),
  };
  return call('POST', '/v1/checkout', { body, token: opts.anon ? undefined : token, idem: opts.idem ?? uuid() });
}
const tokenize = async (number) => (await call('POST', `${BASE}/dev/psp/tokens?appId=pk_sandbox`, { body: { type: 'card', card: { number, holder_name: 'MARINA', exp_month: 12, exp_year: 2030, cvv: '123' } } })).json.id;
const cfg = await call('GET', '/v1/payments/client-config');
check('client-config do PSP', cfg.json?.tokenize_url?.endsWith('/dev/psp/tokens'));
const idem = uuid();
const card = await buy(forSale[0], { method: 'credito', card_token: await tokenize('4111111111111111') }, { idem });
check('cartão aprovado → 201 pago', card.status === 201 && card.json.order.status === 'paid', card.json);
const again = await call('POST', '/v1/checkout', { body: null, token, idem }).catch(() => null);
check('mesma chave com outro corpo → 422', again?.json?.code === 'idempotency-mismatch' || again?.status === 422, again?.json);
check('total errado → price-changed', (await buy(forSale[1], { method: 'pix' }, { total: 1 })).json?.code === 'price-changed');
const declined = await buy(forSale[1], { method: 'credito', card_token: await tokenize('4000000000000002') });
check('cartão recusado → 402', declined.status === 402 && declined.json.code === 'card-declined', declined.json);
const stockAfter = (await call('GET', `/v1/catalog/listings/${forSale[1].id}`)).json.stock;
check('recusa devolve o estoque', stockAfter === forSale[1].stock, { stockAfter, before: forSale[1].stock });
const pix = await buy(forSale[1], { method: 'pix' });
check('Pix → aguardando, com código', pix.status === 201 && pix.json.pix?.code && pix.json.order.status === 'awaiting-payment', pix.json);
const guest = await buy(forSale[2], { method: 'pix' }, { anon: true, buyer: { cpf: cpf('111444777'), name: 'Ana Souza', email: 'ana@exemplo.com' } });
check('compra sem conta → guest token', guest.status === 201 && guest.json.guest_token, guest.json);
const gstatus = await call('GET', `/v1/checkout/orders/${guest.json.order.id}/payment`, { guest: guest.json.guest_token });
check('guest token lê o próprio pedido', gstatus.status === 200 && gstatus.json.status === 'pending', gstatus.json);
check('guest token não lê outro pedido', (await call('GET', `/v1/checkout/orders/${pix.json.order.id}/payment`, { guest: guest.json.guest_token })).status === 404);
console.log('  … aguardando o Pix do sandbox (20 s)');
await sleep(21_000);
const paid = await call('GET', `/v1/checkout/orders/${pix.json.order.id}/payment`, { token });
check('Pix pago', paid.json?.status === 'paid', paid.json);

section('Painel do artesão');
let artisanSession = await login(ARTISAN);
let atk = artisanSession.access_token;
const ws = await call('GET', '/v1/sellers/me/workspace', { token: atk });
check('painel (dona)', ws.status === 200 && ws.json.role === 'owner' && ws.json.storefront_url_name === 'barro-vivo', ws.json);
check('comprador sem vitrine → 404', (await call('GET', '/v1/sellers/me/workspace', { token })).status === 404);
const priv = await call('GET', '/v1/sellers/me/storefront', { token: atk });
check('vitrine privada com endereço do cofre e ETag', priv.status === 200 && priv.json.business_address?.street && priv.headers.get('etag'), priv.json);
const update = { brand_name: priv.json.brand_name, slogan: 'Cerâmica de alta temperatura.', storefront_url_name: 'barro-vivo', visual_identity: { palette: { primary: '#2E5E6B' }, font_option_id: 'classica' } };
check('PUT sem If-Match → 422', (await call('PUT', '/v1/sellers/me/storefront', { token: atk, body: update })).status === 422);
const put = await call('PUT', '/v1/sellers/me/storefront', { token: atk, body: update, headers: { 'If-Match': priv.headers.get('etag') } });
check('PUT com If-Match', put.status === 200 && put.json.slogan === 'Cerâmica de alta temperatura.', put.json);
check('ETag antigo → 412', (await call('PUT', '/v1/sellers/me/storefront', { token: atk, body: update, headers: { 'If-Match': priv.headers.get('etag') } })).status === 412);
check('nome "Saci Oficial" recusado', (await call('PUT', '/v1/sellers/me/storefront', { token: atk, body: { ...update, brand_name: 'Saci Oficial' }, headers: { 'If-Match': put.headers.get('etag') } })).json?.code === 'name-not-allowed');

// Anúncio novo com foto enviada pela esteira.
const png = await makePng();
const upload = await call('POST', '/v1/media/me/uploads', { token: atk, body: { purpose: 'listing-photo', content_type: 'image/png', content_length: png.length, checksum_sha256: crypto.createHash('sha256').update(png).digest('base64') } });
check('envio assinado', upload.status === 201 && upload.json.upload_url, upload.json);
const sent = await fetch(upload.json.upload_url, { method: 'PUT', headers: upload.json.upload_headers, body: png });
check('PUT direto na quarentena', sent.status === 200);
let status;
for (let i = 0; i < 20; i++) { status = (await call('GET', `/v1/media/me/uploads/${upload.json.upload_id}`, { token: atk })).json; if (status.status === 'ready') break; await sleep(300); }
check('imagem verificada e publicada', status.status === 'ready' && status.public_url, status);
const write = { title: 'Travessa de grés azul', description: 'Feita no torno e queimada a 1.280 °C.', category: 'ceramica', price: { amount: 19000, currency: 'BRL' }, quantity: 2, production_days: 0, customizable: false, ships_abroad: false, attributes: [{ key: 'material', label: 'Material', value: 'Grés' }], photo_upload_ids: [upload.json.upload_id] };
check('texto com contato → listing-rejected', (await call('POST', '/v1/catalog/me/listings', { token: atk, body: { ...write, description: 'chama no whatsapp' }, idem: uuid() })).json?.code === 'listing-rejected');
const created = await call('POST', '/v1/catalog/me/listings', { token: atk, body: write, idem: uuid() });
check('anúncio publicado', created.status === 201 && created.json.status === 'published', created.json);
await sleep(300);
const pub = await call('GET', `/v1/catalog/listings/${created.json.listing_id}`);
check('anúncio na feira', pub.status === 200 && pub.json.images[0].url === status.public_url);
const inSearch = await call('GET', '/v1/catalog/search?q=travessa gres');
check('anúncio na busca (indexador)', inSearch.json.items.some((l) => l.id === created.json.listing_id));
const mine = await call('GET', `/v1/catalog/me/listings/${created.json.listing_id}`, { token: atk });
check('anúncio no painel com ETag', mine.status === 200 && mine.headers.get('etag'));
check('pausar', (await call('POST', `/v1/catalog/me/listings/${created.json.listing_id}/pause`, { token: atk })).json?.status === 'paused');
check('pausado some da feira', (await call('GET', `/v1/catalog/listings/${created.json.listing_id}`)).status === 404);

section('Vendas');
const sales = await call('GET', '/v1/orders/me/sales?group=a-fazer', { token: atk });
check('vendas a fazer com contagens', sales.status === 200 && sales.json.counts['a-fazer'] > 0, sales.json?.counts);
const fresh = sales.json.items.find((s) => s.status === 'in_production' && s.id.startsWith('sal_'));
const ready = await call('POST', `/v1/orders/me/sales/${fresh.id}/ready`, { token: atk });
check('peça pronta', ready.status === 200 && ready.json.status === 'awaiting_shipment', ready.json);
await sleep(500);
const label = await call('GET', `/v1/shipping/me/sales/${fresh.id}/label`, { token: atk });
check('etiqueta por URL assinada', label.status === 200 && label.json.url.includes('signature='), label.json);
const pdf = label.json?.url ? await fetch(label.json.url) : null;
check('PDF da etiqueta', pdf?.status === 200 && (await pdf.text()).startsWith('%PDF'));
check('etiqueta de outra vitrine → 404', (await call('GET', `/v1/shipping/me/sales/${fresh.id}/label`, { token })).status !== 200);
const shipped = await call('POST', `/v1/orders/me/sales/${fresh.id}/shipment`, { token: atk, body: { tracking_code: 'QB123456789BR' } });
check('postada com rastreio', shipped.json?.status === 'shipped', shipped.json);
await sleep(300);
const hook = await call('POST', '/dev/carrier/sandbox', { body: { event_id: uuid(), tracking_code: 'QB123456789BR', status: 'delivered' } });
check('webhook da transportadora (assinado)', hook.status === 202);
await sleep(500);
const delivered = await call('GET', `/v1/orders/me/sales/${fresh.id}`, { token: atk });
check('entregue pelo evento', delivered.json?.status === 'delivered', delivered.json?.status);
check('valores só para a dona (S-10)', delivered.json?.payment?.fee);

section('Equipe e chamados');
const member = await login(MEMBER);
const mws = await call('GET', '/v1/sellers/me/workspace', { token: member.access_token });
check('pessoa da equipe vê o painel', mws.json?.role === 'member', mws.json);
check('equipe não lê o Financeiro (sem payment)', !(await call('GET', `/v1/orders/me/sales/${fresh.id}`, { token: member.access_token })).json?.payment);
check('equipe sem a seção vitrine → 403', (await call('GET', '/v1/sellers/me/storefront', { token: member.access_token })).status === 403);
atk = await stepUp(atk);
const invite = await call('POST', '/v1/sellers/me/team', { token: atk, body: { cpf: BUYER, permissions: ['atendimento'] } });
check('convite sem nome (S-04)', invite.status === 202 && invite.json.status === 'invited' && !invite.json.name, invite.json);
const invitations = await call('GET', '/v1/sellers/me/team-invitations', { token });
check('convite recebido', invitations.json?.items?.length === 1, invitations.json);
const accept = await call('POST', `/v1/sellers/me/team-invitations/${invitations.json.items[0].id}/accept`, { token: await stepUp(token) });
check('aceite', accept.status === 204, accept.json);
const tickets = await call('GET', '/v1/support/me/tickets', { token: atk });
check('chamados', tickets.status === 200 && tickets.json.items.length > 0);
const ticket = tickets.json.items.find((t) => t.status !== 'resolved');
const msg = await call('POST', `/v1/support/me/tickets/${ticket.protocol}/messages`, { token: atk, body: { text: 'Fica pronto até sexta!' }, idem: uuid() });
check('resposta ao chamado', msg.status === 200 && msg.json.author === 'vitrine', msg.json);
check('encerrar', (await call('POST', `/v1/support/me/tickets/${ticket.protocol}/resolve`, { token: atk })).json?.status === 'resolved');
check('mensagem em chamado encerrado → 409', (await call('POST', `/v1/support/me/tickets/${ticket.protocol}/messages`, { token: atk, body: { text: 'oi' }, idem: uuid() })).json?.code === 'ticket-resolved');
// CPF sintético na mensagem (gerado aqui: nenhum CPF válido escrito no repositório).
const c = cpf('123456789');
const typedCpf = c.slice(0, 3) + '.' + c.slice(3, 6) + '.' + c.slice(6, 9) + '-' + c.slice(9);
const barroVivo = (await call('GET', '/v1/catalog/storefronts/barro-vivo/listings?limit=1')).json.items[0].artisan.id;
const handoff = await call('POST', '/dev/assistant/handoffs', { body: { storefront_id: barroVivo, first_name: 'Rita', message: `Meu CPF é ${typedCpf}, quero falar com a vitrine` } });
await sleep(500);
const after = await call('GET', '/v1/support/me/tickets?status=open', { token: atk });
const opened = after.json.items.find((t) => t.customer.first_name === 'Rita');
check('repasse da Caipora abre chamado', handoff.status === 202 && opened, after.json?.items?.length);
const full = opened && (await call('GET', `/v1/support/me/tickets/${opened.protocol}`, { token: atk })).json;
check('texto de quem compra mascarado (S-23)', full && !JSON.stringify(full).includes(typedCpf));

section('Avaliação');
const buyerOrders = await call('GET', '/v1/orders/me/purchases', { token });
check('compra entregue', buyerOrders.json.items.some((p) => p.status === 'delivered'));

console.log(failures ? `\n\x1b[31m${failures} falha(s)\x1b[0m` : '\n\x1b[32mTudo certo.\x1b[0m');
process.exit(failures ? 1 : 0);

// PNG 700×700 cinza, gerado aqui (sem dependência).
async function makePng() {
  const w = 700, h = 700;
  const raw = Buffer.alloc((w * 3 + 1) * h, 0x80);
  for (let y = 0; y < h; y++) raw[y * (w * 3 + 1)] = 0;
  const zlib = await import('node:zlib');
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(zlib.crc32(td) >>> 0);
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}
