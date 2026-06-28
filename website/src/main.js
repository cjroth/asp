// Mobile nav toggle
const nav = document.querySelector('.pa-nav')
const toggle = document.querySelector('.menu-toggle')
toggle?.addEventListener('click', () => nav?.classList.toggle('open'))

// Close the mobile menu after tapping a link
nav?.querySelectorAll('.nav-links a').forEach((a) =>
  a.addEventListener('click', () => nav.classList.remove('open'))
)
