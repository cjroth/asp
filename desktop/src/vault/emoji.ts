// Emoji dataset for the Customize-vault icon picker. Vendored verbatim from the
// design's `emoji-data.js` (the design read it off `window.ASP_EMOJI`); here it
// is a typed module so the picker and its tests import it directly. Shape per
// category: { name, icon, emojis: [[char, keywords], ...] }.

export type EmojiPair = [string, string];
export interface EmojiCategory {
  name: string;
  icon: string;
  emojis: EmojiPair[];
}

export const EMOJI_CATEGORIES: EmojiCategory[] = [
  { name: 'Smileys', icon: '😀', emojis: [
    ['😀','grin happy'],['😃','smile happy'],['😄','smile laugh'],['😁','beam grin'],['😆','laugh'],['😅','sweat laugh'],['🤣','rofl laugh'],['😂','joy tears'],['🙂','slight smile'],['🙃','upside down'],['😉','wink'],['😊','blush smile'],['😇','angel halo'],['🥰','love hearts'],['😍','heart eyes love'],['🤩','star struck'],['😘','kiss'],['😋','yum tasty'],['😛','tongue'],['🤪','zany silly'],['🤔','thinking'],['🤨','raised brow'],['😐','neutral'],['😶','no mouth'],['🙄','eye roll'],['😏','smirk'],['😬','grimace'],['😌','relieved calm'],['😴','sleep zzz'],['😪','sleepy'],['🤤','drool'],['😷','mask sick'],['🤒','thermometer sick'],['🤕','hurt bandage'],['🥳','party celebrate'],['🥺','pleading'],['😎','cool sunglasses'],['🤓','nerd geek'],['🧐','monocle'],['😕','confused'],['😟','worried'],['🙁','frown'],['😮','surprised'],['😯','hushed'],['😲','astonished'],['😳','flushed'],['🥹','holding tears'],['😢','cry sad'],['😭','sob cry'],['😤','triumph huff'],['😠','angry'],['😡','rage mad'],['🤯','mind blown'],['😱','scream fear'],['😨','fearful'],['😰','anxious'],['🤗','hug'],['🤭','giggle oops'],['🤫','shush quiet'],['😶‍🌫️','foggy'],
  ]},
  { name: 'People', icon: '👋', emojis: [
    ['👋','wave hello'],['🤚','raised hand'],['✋','hand stop'],['🖐️','hand fingers'],['👌','ok'],['🤌','pinch'],['🤏','small pinch'],['✌️','peace victory'],['🤞','crossed fingers luck'],['🫰','fingers crossed'],['🤟','love you'],['🤘','rock horns'],['👈','point left'],['👉','point right'],['👆','point up'],['👇','point down'],['☝️','index up'],['👍','thumbs up like'],['👎','thumbs down'],['✊','fist'],['👊','punch fist'],['👏','clap'],['🙌','raise hands'],['👐','open hands'],['🤲','palms'],['🙏','pray thanks'],['✍️','write hand'],['💪','muscle strong'],['🧠','brain mind'],['👀','eyes look'],['🫶','heart hands'],['🤝','handshake deal'],['👤','silhouette person'],['👥','people'],['🧑','person'],['👶','baby'],['🧒','child'],['🧓','older'],['👨‍💻','developer coder'],['👩‍💻','developer coder'],['🦾','mechanical arm'],['🫡','salute'],
  ]},
  { name: 'Animals', icon: '🐶', emojis: [
    ['🐶','dog'],['🐱','cat'],['🐭','mouse'],['🐹','hamster'],['🐰','rabbit bunny'],['🦊','fox'],['🐻','bear'],['🐼','panda'],['🐨','koala'],['🐯','tiger'],['🦁','lion'],['🐮','cow'],['🐷','pig'],['🐸','frog'],['🐵','monkey'],['🐔','chicken'],['🐧','penguin'],['🐦','bird'],['🦅','eagle'],['🦉','owl'],['🦇','bat'],['🐺','wolf'],['🐗','boar'],['🐴','horse'],['🦄','unicorn'],['🐝','bee'],['🐛','bug'],['🦋','butterfly'],['🐌','snail'],['🐞','ladybug'],['🐢','turtle'],['🐍','snake'],['🐙','octopus'],['🐳','whale'],['🐬','dolphin'],['🐟','fish'],['🦈','shark'],['🌵','cactus'],['🌲','tree evergreen'],['🌳','tree'],['🌴','palm'],['🌱','seedling sprout'],['🌿','herb leaf'],['🍀','clover luck'],['🍁','maple leaf'],['🌸','blossom'],['🌹','rose'],['🌻','sunflower'],['🌼','flower'],['🌷','tulip'],['🪴','plant pot'],
  ]},
  { name: 'Food', icon: '🍎', emojis: [
    ['🍎','apple'],['🍏','green apple'],['🍐','pear'],['🍊','orange'],['🍋','lemon'],['🍌','banana'],['🍉','watermelon'],['🍇','grapes'],['🍓','strawberry'],['🫐','blueberry'],['🍒','cherry'],['🍑','peach'],['🥭','mango'],['🍍','pineapple'],['🥥','coconut'],['🥝','kiwi'],['🍅','tomato'],['🥑','avocado'],['🥦','broccoli'],['🥕','carrot'],['🌽','corn'],['🌶️','pepper hot'],['🥔','potato'],['🍞','bread'],['🥐','croissant'],['🥨','pretzel'],['🧀','cheese'],['🥚','egg'],['🍳','fried egg'],['🥞','pancakes'],['🥓','bacon'],['🍔','burger'],['🍟','fries'],['🍕','pizza'],['🌭','hotdog'],['🌮','taco'],['🌯','burrito'],['🍜','noodles ramen'],['🍣','sushi'],['🍱','bento'],['🍙','rice ball'],['🍦','ice cream'],['🍩','donut'],['🍪','cookie'],['🎂','cake birthday'],['🍰','cake'],['🍫','chocolate'],['🍬','candy'],['🍿','popcorn'],['☕','coffee'],['🍵','tea'],['🧃','juice'],['🍺','beer'],['🍷','wine'],['🥂','cheers'],
  ]},
  { name: 'Activity', icon: '⚽', emojis: [
    ['⚽','soccer football'],['🏀','basketball'],['🏈','football'],['⚾','baseball'],['🎾','tennis'],['🏐','volleyball'],['🏉','rugby'],['🎱','pool 8ball'],['🏓','ping pong'],['🏸','badminton'],['🥅','goal'],['🏒','hockey'],['🏑','field hockey'],['🥍','lacrosse'],['🏏','cricket'],['⛳','golf'],['🏹','archery bow'],['🎣','fishing'],['🥊','boxing'],['🥋','martial arts'],['⛸️','skating'],['🎿','ski'],['🛷','sled'],['🏂','snowboard'],['🏋️','weightlifting gym'],['🤸','cartwheel'],['🤺','fencing'],['🤾','handball'],['🏌️','golfer'],['🏄','surf'],['🏊','swim'],['🚴','cycling bike'],['🧗','climb'],['🎯','target dart'],['🎮','game controller'],['🕹️','joystick'],['🎲','dice'],['🧩','puzzle'],['♟️','chess'],['🎭','theater arts'],['🎨','art palette'],['🎬','movie clapper'],['🎤','mic sing'],['🎧','headphones'],['🎼','music score'],['🎹','piano keys'],['🥁','drum'],['🎸','guitar'],['🎺','trumpet'],['🎻','violin'],['🎷','sax'],
  ]},
  { name: 'Travel', icon: '✈️', emojis: [
    ['🚗','car'],['🚕','taxi'],['🚙','suv'],['🚌','bus'],['🏎️','race car'],['🚓','police car'],['🚑','ambulance'],['🚒','fire truck'],['🚐','van'],['🛻','pickup'],['🚚','truck'],['🚜','tractor'],['🏍️','motorcycle'],['🛵','scooter'],['🚲','bicycle'],['🛴','kick scooter'],['🚂','train'],['🚆','train'],['🚇','metro subway'],['🚊','tram'],['✈️','plane flight'],['🛫','takeoff'],['🚀','rocket launch'],['🛸','ufo'],['🚁','helicopter'],['⛵','sailboat'],['🚤','speedboat'],['🛥️','motor boat'],['🚢','ship'],['⚓','anchor'],['🗺️','map'],['🧭','compass'],['🏔️','mountain'],['⛰️','mountain'],['🌋','volcano'],['🏕️','camping'],['🏖️','beach'],['🏝️','island'],['🏜️','desert'],['🏠','house home'],['🏡','home garden'],['🏢','office building'],['🏬','store'],['🏫','school'],['🏥','hospital'],['🏦','bank'],['🏰','castle'],['🗼','tower'],['🗽','statue liberty'],['🌁','foggy city'],['🌃','night city'],['🌉','bridge'],['🏙️','cityscape'],['🌅','sunrise'],['🌄','sunrise mountain'],
  ]},
  { name: 'Objects', icon: '💡', emojis: [
    ['💡','idea bulb light'],['🔦','flashlight'],['🕯️','candle'],['🔋','battery'],['🔌','plug power'],['💻','laptop computer'],['🖥️','desktop monitor'],['🖨️','printer'],['⌨️','keyboard'],['🖱️','mouse'],['💾','floppy save disk'],['💿','disc cd'],['📀','dvd'],['📱','phone mobile'],['☎️','telephone'],['📞','phone receiver'],['📟','pager'],['📠','fax'],['📺','tv'],['📷','camera'],['📸','camera flash'],['📹','video camera'],['🎥','movie camera'],['🔍','search magnify'],['🔎','search'],['🔭','telescope'],['🔬','microscope'],['📡','satellite dish'],['💵','money cash'],['💳','card credit'],['🧾','receipt'],['🔑','key'],['🗝️','old key'],['🔒','lock secure'],['🔓','unlock'],['🔐','locked key'],['🛡️','shield'],['🔧','wrench tool'],['🔨','hammer'],['🛠️','tools'],['⚙️','gear settings'],['🧰','toolbox'],['🧲','magnet'],['🧪','test tube science'],['🧫','petri lab'],['🧬','dna'],['📦','box package'],['📫','mailbox'],['📮','postbox'],['✉️','envelope mail'],['📧','email'],['📝','memo note write'],['📄','page document'],['📃','page'],['📑','tabs bookmark'],['📊','bar chart'],['📈','chart up growth'],['📉','chart down'],['📅','calendar'],['📆','calendar'],['🗓️','calendar'],['📋','clipboard'],['📌','pin'],['📍','location pin'],['📎','paperclip'],['🖇️','clips'],['📐','ruler triangle'],['📏','ruler'],['✂️','scissors'],['🗂️','folder dividers'],['📁','folder'],['📂','folder open'],['🗄️','file cabinet'],['🗃️','card box'],['🗳️','ballot'],['📚','books'],['📖','book open'],['📕','red book'],['📗','green book'],['📘','blue book'],['📙','orange book'],['📓','notebook'],['📔','notebook decorated'],['📒','ledger'],['🔖','bookmark'],['🏷️','tag label'],['✏️','pencil'],['✒️','pen nib'],['🖊️','pen'],['🖋️','fountain pen'],['🖌️','brush'],['🖍️','crayon'],['⏰','alarm clock'],['⏱️','stopwatch'],['⌛','hourglass'],['🧭','compass'],
  ]},
  { name: 'Symbols', icon: '❤️', emojis: [
    ['❤️','red heart love'],['🧡','orange heart'],['💛','yellow heart'],['💚','green heart'],['💙','blue heart'],['💜','purple heart'],['🖤','black heart'],['🤍','white heart'],['🤎','brown heart'],['💔','broken heart'],['❣️','heart exclaim'],['💕','two hearts'],['💞','revolving hearts'],['💓','beating heart'],['💗','growing heart'],['💖','sparkle heart'],['💘','cupid arrow heart'],['💝','heart gift'],['⭐','star'],['🌟','glowing star'],['✨','sparkles'],['⚡','lightning bolt'],['🔥','fire flame'],['💥','boom collision'],['💫','dizzy star'],['☀️','sun'],['🌙','moon'],['☄️','comet'],['🌈','rainbow'],['☁️','cloud'],['❄️','snowflake'],['💧','droplet water'],['🌊','wave ocean'],['✅','check done'],['☑️','checkbox'],['✔️','check'],['❌','cross x'],['❎','cross mark'],['➕','plus add'],['➖','minus'],['➗','divide'],['✖️','multiply'],['❓','question'],['❗','exclaim'],['‼️','double exclaim'],['💯','hundred perfect'],['🔔','bell'],['🔕','mute bell'],['🎵','note music'],['🎶','notes music'],['💬','speech bubble chat'],['💭','thought bubble'],['🗯️','anger bubble'],['♻️','recycle'],['⚠️','warning'],['🚫','no forbidden'],['🔰','beginner'],['⚜️','fleur'],['🔱','trident'],['✳️','asterisk'],['❇️','sparkle'],['©️','copyright'],['®️','registered'],['™️','trademark'],['🆕','new'],['🆗','ok'],['🆒','cool'],['🔝','top up'],['🔄','refresh cycle'],['🔁','repeat loop'],['▶️','play'],['⏸️','pause'],['⏹️','stop'],['🎯','target'],['🏁','flag finish'],['🚩','flag'],['🏳️','white flag'],['🏴','black flag'],['🏆','trophy win'],['🥇','gold medal first'],['🎖️','medal'],['🏅','medal sport'],['🎗️','ribbon'],['🎀','bow ribbon'],['🎁','gift present'],['🎉','party tada'],['🎊','confetti'],['🎈','balloon'],['👑','crown'],['💎','diamond gem'],
  ]},
];

// The picker's emoji list: when there's a query, search every category's
// name+keywords; otherwise show the active category. Mirrors design lines 2082–2088.
export function emojiResults(query: string, catIndex: number): string[] {
  const q = query.trim().toLowerCase();
  if (q) {
    const out: string[] = [];
    for (const g of EMOJI_CATEGORIES) {
      for (const [char, keywords] of g.emojis) {
        if ((g.name + ' ' + keywords).toLowerCase().indexOf(q) !== -1) out.push(char);
      }
    }
    return out;
  }
  const grp = EMOJI_CATEGORIES[Math.min(catIndex, EMOJI_CATEGORIES.length - 1)] || EMOJI_CATEGORIES[0];
  return grp.emojis.map((p) => p[0]);
}
