# Legacy Knowledge: keywest.health

Imported from legacy Qdrant database. These are general facts and rules.

## Memory 04b3252e-42a8-4561-8100-4d5674ca6550
All typography and font families for the "Quiet Tropical Luxury" Key West aesthetic must be sourced from Google Fonts (https://fonts.google.com/). The selected fonts should align with the 'White Linen & Deep Ocean' visual identity (Historic Key West elegance, upscale but casual, explicitly 'not Miami').

## Memory 1741920e-466d-4f8c-82fb-697a8fab1b0d
**Architectural Clarification on Frameworks (React & SSG vs. SPA):**
The user clarified their stance on JavaScript frameworks.
*   **The Misconception:** The user is *not* anti-React. They are anti-SPA (Single Page Application).
*   **The Mandate:** The architecture must be a static website because the content is primarily static, but the user understands that *interactive components* (booking calendars, complex sliders, calculators) might require React.
*   **The Strategic Shift:** The user stated, *"AI makes a CMS redundant, that is why I am starting this project."* The core thesis of the Key West Health rebuild is that a headless LLM agent reading a local `ontology.json` completely replaces the need for a traditional visual CMS like Wix or WordPress.
*   **Astro vs Eleventy Consideration:** If the user is open to React for *islands of interactivity* but demands a static HTML site for SEO, **Astro** is the superior architectural choice over Eleventy. Astro generates pure zero-JS HTML by default but allows the developer to drop in a single React component (e.g., `<BookingWidget client:load />`) only where needed.

## Memory 225ff898-c59e-49a1-a498-cc07879b3979
**Social Media Centralization Architecture:**
To avoid the chaotic, disjointed "post-everywhere-manually" strategy, the Key West Health platform must adopt an Omnichannel Centralized Hub.

1. **The Hub (Local Database):** Instead of logging into Facebook, Instagram, and Google Business Profile separately, the practice creates a single "Social Post" inside the local system (or headless CMS).
2. **The Distribution Engine:** Because official MCP servers for Meta (Facebook/Instagram) require complex OAuth App approvals, the MarTech Integrator will build an automated pipeline using a Webhook service (like Make.com or Zapier).
3. **The Workflow:** The `Social Media & Community Strategist` drafts the post. When approved, it is saved. The system automatically blasts it to Facebook, Instagram, and Google Business Profile simultaneously.

**Site Scraping Results:**
The Wix homepage revealed several other generically named static pages that likely contain hardcoded data not present in the CMS:
* `/services-4` (Likely Aesthetics overview)
* `/services-5-1` (Likely Primary Care overview)
* `/general-8` (Robotic Hair Transplant - *Already recovered*)
* `/shop` (Ecommerce/Products)
* `/about` (Practice history/Bios)

## Memory 359b0d92-c4f0-482f-a3d8-054fc5fdeb27
**Architecture Rule: Static-First & Micro-Services for Key West Health**

1.  **Core Philosophy:** The Key West Health website must remain a 100% Static Site Generation (SSG) Astro build. Do NOT convert the Astro application to Server-Side Rendering (SSR) or introduce a heavy Node.js/Docker runtime for the frontend.
2.  **Dynamic Elements (Micro-Frontends):** Any required dynamic functionality (e.g., Contact Forms, Booking Inquiries, Newsletter Signups) must be implemented as lightweight, client-side JavaScript components (e.g., using standard HTML `<form>` with a `fetch()` call, or tiny isolated Preact/Svelte islands if strictly necessary).
3.  **Backend Integration (Serverless Endpoints):** All dynamic client-side components must submit data to isolated, single-purpose Serverless endpoints (e.g., Google Cloud Functions or Firebase Cloud Functions). 
4.  **Data Consolidation:** These serverless functions are responsible for validation, spam prevention, and securely routing the data into Google Cloud's centralized ecosystem (e.g., Firestore or BigQuery) to support future business intelligence and AI analysis.
5.  **Cost & Security Imperative:** This architecture guarantees maximum frontend performance (CDN-cached HTML), zero attack surface on the web server, and leverages the massive free tiers of serverless compute, avoiding base fees for idle runtimes or load balancers.

## Memory 399d5900-0aa4-426e-b153-e03537b5ce6c
The AI Robotic Hair Transplant (HARRTS FUEsion X / I Brain Robotics) is the highest-value, most exclusive service the practice offers, but it was entirely missing from the Wix CMS database exports because it was manually built into a static Wix page named `/general-8`.

**SEO Correction Strategy:**
1. The URL `/general-8` is SEO poison. The new static site must route this to `/hair-restoration/robotic-transplant-key-west` or similar high-intent URL.
2. Dr. Shannon Smeltzer's specific credentials (DNP, ASAHRS Fellow) must be explicitly linked to this service in the data ontology to satisfy Google E-E-A-T requirements for YMYL medical searches.
3. This service must be structurally separated from lower-tier "Hair Restoration" (like PRP/Vampire) in the site's ontology so it does not get cannibalized by low-intent search terms.

## Memory 45a2b445-80a4-4963-98e1-3961bfb99fe0
Long-term Asset Strategy: Replace manual vendor asset downloads (Allergan, Galderma, BTL, Merz) by building custom MCPs capable of authenticating into their respective provider media portals and automatically pulling down the latest marketing kits, copy, and before/after photos into the Astro project.

## Memory 461ed089-ef2f-411e-b6ac-eb983b41805b
**Image Sourcing Constraint:** 
The AI cannot currently scrape the Wix Free Media Library because the Wix MCP server and the direct Wix REST API (`/wix-data/v2`) only provide access to the user's specific CMS collections and databases, not the global Wix stock photo engine.
*   **Resolution:** The AI must rely on high-quality Unsplash URLs for wireframing. The human user will need to ultimately swap these placeholder URLs with their preferred licensed photos from the Wix Media Library or their own camera roll when moving to production.

## Memory 53bf8168-e908-404f-ad3f-1844e7f9ce00
The Web Marketing Agency Panel (WMAP) has been formally installed as a global skill at `~/.config/opencode/skills/wmap`. It is now accessible in any project via the `/wmap` command. 

The panel includes:
1. Visionary Director
2. SEO & Content Strategist
3. Social Media & Community Strategist
4. Conversion Optimizer
5. Data Analyst
6. MarTech Integrator
7. Brand Guardian

## Memory 575ac6ce-8537-4f1c-9457-a7ae3ae4ed2e
**Evaluation of `Taste Skill` (tasteskill.dev):**
The user requested an evaluation of `https://www.tasteskill.dev/`.
*   **What it is:** An "Anti-Slop" frontend framework for AI agents. It uses a `SKILL.md` file to force AI agents to stop using generic UI patterns (e.g., standard Bootstrap blues, gradient buttons, 3-column emoji grids) and instead enforces a highly opinionated, premium design language.
*   **The Conflict:** Taste Skill is hyper-opinionated toward "Dark Premium OLED" (`#0e1011` black canvas, cinematic dark mode).
*   **Value for Key West Health:** We absolutely **must not** install this skill. Key West Health's established brand identity is "White Linen, Bright Tropical, Aerial and Airy." If we install Taste Skill, it will actively fight our `DESIGN.md` file and attempt to turn the medical practice website into a dark-mode cyberpunk/cinematic experience. 
*   **Takeaway:** The *philosophy* of Taste Skill (using markdown to force the AI out of generic slop) is exactly what we are already doing with our own `DESIGN.md`. We have essentially built our own "Key West Taste Skill" locally. We do not need theirs.

## Memory 6b52dd72-07a9-43b7-a02c-7cadfddff2a4
**Design System Documentation (DESIGN.md):**
The project will adopt the `DESIGN.md` paradigm (as popularized by Google Stitch / awesome-design-md). 
Instead of the AI agents guessing at Hex codes and fonts every time they generate a page, the MarTech Integrator and Brand Guardian will rely on a centralized `DESIGN.md` file sitting in the local `keywest.health` project root. This file will contain the exact "White Linen, Tropical Green, and Orchid Purple" design tokens, typography rules, and component structures (like the Glassmorphism overlay). This guarantees that any future AI agent running an autonomous campaign within this specific repository will generate pixel-perfect, on-brand HTML without needing human correction.

## Memory 6c4dd21a-cd69-4315-adc8-db2d238e2ee8
The stock-images-mcp (https://github.com/Zulelee/stock-images-mcp) repository has been reviewed and determined safe for use. It uses standard HTTP libraries to interact with Unsplash, Pixabay, and Pexels and has no obvious exploits. It should be configured locally in the project's .opencode.json configuration rather than the global environment.

## Memory 7b7f588a-abe3-41d1-a626-1d07806ee482
**Evaluation of `21st.dev` (UI Components Library):**
The user requested an evaluation of `https://21st.dev/home` (where they previously found the parallax scrolling inspiration).
*   **What it is:** A modern registry of high-end UI components, AI agent templates, and interaction designs. It heavily features components built with React, Next.js, Tailwind, and Framer Motion.
*   **Value for Key West Health:** It is **highly valuable as an inspiration board (Mood Board).** The user can browse 21st.dev to find specific micro-interactions, layouts, or effects they love (like the parallax scroll).
*   **Architectural Constraint:** We cannot copy-paste the code directly from 21st.dev. Because we explicitly chose an Eleventy (11ty) Jamstack architecture for maximum SEO speed and unbreakable security, we are avoiding heavy React/Next.js dependencies. When the user finds a React component on 21st.dev they like, the MarTech Integrator must manually translate its logic into Vanilla JS and pure HTML/Tailwind (exactly as we did for the V5 wireframe parallax effect).

## Memory 85dfb706-6a53-4c23-b684-062b3dff846d
The frontend for keywest.health is a pure Jamstack architecture using Eleventy (11ty) for Static Site Generation (SSG). 

We explicitly chose NOT to use a heavy framework like React or Next.js. The goal is to output pure, fast-loading, unbreakable static HTML/CSS pages.

**Core Rules:**
1.  **Single Source of Truth:** All global brand elements (phone numbers, taglines, headshots) must be stored in `frontend/src/_data/global.json` (or pulled dynamically from the local `wix_cms.db`).
2.  **No Hardcoding:** Templates (e.g., `src/index.html`) must use Eleventy's data cascading (e.g., `{{ global.tagline }}`) to ensure site-wide consistency when the AI or user updates the brand strategy.
3.  **Analytics Retention:** The static templates must include snippets for client-side tracking (Google Analytics, Meta Pixels, etc.) to ensure the Data Analyst agent retains visibility into marketing performance. The transition to static HTML does NOT mean losing analytics.

## Memory 8e4690eb-326e-452a-810e-05b8647944d0
**Design Automation & Component Scaling:**
The Key West Health project aims to heavily automate design iteration without a heavy visual builder like Paperclip.
The user discovered `awesome-design-md` (VoltAgent), indicating a strong desire for "Design-to-Code" capabilities using Markdown/AI generation.
*   **Strategy Integration:** The Eleventy static site architecture must be built using modular, highly semantic HTML/Tailwind components. This ensures that when the Visionary Director or UX Optimizer drafts a new landing page or campaign in standard Markdown (`.md`), the MarTech Integrator can instantly compile it into perfect, production-ready "White Linen" Tailwind HTML, effectively mirroring the `awesome-design-md` philosophy.

## Memory 8ebd885b-fc8e-48da-b30d-2be3436c4e14
**Aesthetic Refinement: White Linen & Gold (Quiet Tropical Luxury)**
The design system must strictly avoid heavy, dark backgrounds. The vibe is "White Linen" – bright, airy, clean, and breathable. 
*   **Backgrounds:** Crisp white linen or very soft ivory.
*   **Accents:** Gold (representing luxury and warmth) paired with the specific colors from the Key West Health logo.
*   **Brand Emotion:** "High-class tropical" or "Luxury but Casual." This is the definition of Key West wealth: affluent but relaxed, upscale but approachable.
*   **Typography:** Thin, elegant serif fonts for headings (gold or deep logo colors) and clean sans-serif for high-legibility medical information.

## Memory aa6d1cd1-0469-4957-aa44-a94c264d9df3
**WMAP Local Skill Setup:**
Per user request, the Web Marketing Agency Panel (WMAP) has been removed from the global skills directory (`~/.config/opencode/skills/wmap`) and placed locally inside the project at `.opencode/skills/wmap/`. This ensures the skill and its agent profiles remain scoped exclusively to the `keywest.health` repository.

## Memory d563d98f-491e-4f80-8d24-ce7f4ead4dd1
**Evaluation of `ui-ux-pro-max-skill`:**
The user requested an evaluation of `https://github.com/nextlevelbuilder/ui-ux-pro-max-skill`.
*   **What it is:** An AI coding skill that uses a Python backend script to generate complete `DESIGN.md` files (colors, typography, patterns) based on 161 industry-specific reasoning rules.
*   **Verdict:** While extremely powerful for *starting a new project from scratch without a vision*, it is **not needed** for Key West Health. We have already manually curated and locked in the exact aesthetic ("White Linen & Deep Ocean" with specific typography and glassmorphism components) in our own `DESIGN.md` file.
*   **Action:** Do not install this skill. It requires Python dependencies, adds CLI bloat, and risks overriding our highly bespoke, locally tailored design system with generic "Health/Spa" templates.

## Memory e45d047c-b20e-4295-b121-b139d96afc8c
**Long-Term Vision: Fully Autonomous "Autopilot" Marketing Agency**
The ultimate goal for Key West Health is not just a static website, but a fully autonomous marketing machine. Once trust is established, the panel (Visionary, Social, SEO, Data) must be capable of:
1. Automatically designing full-funnel marketing campaigns (e.g., "Summer Hydration Special").
2. Generating the copy, imagery, and landing pages dynamically.
3. Publishing daily social media posts (via webhooks to Facebook/IG/GBP) without human intervention.
4. Monitoring the analytics and adjusting spend/strategy.

**Architectural Implication:** The current technical stack (Eleventy static site + JSON data + Webhooks) perfectly supports this. The AI can programmatically rewrite `specials.json`, trigger a site rebuild, and fire a webhook to Zapier to blast the social channels—all via a single cron job or scheduled Task agent prompt. The architecture must remain entirely text/code-based (no visual drag-and-drop builders) to allow for this future programmatic autonomy.

## Memory fa075b4e-73ad-4288-851a-fb011e25fdf7
### Key West Health - Core Business Strategy & Architecture

**The Primary Goal:** Dominating local search rankings (SEO) above all Key West competitors for every specific service, product, and concern. 
**The Aesthetic:** "Historic Tropical Elegance" - Old Town Key West charm (Charleston/Savannah/New Orleans influence with a Cuban tropical vibe). Upscale, warm, not clinical, absolutely "Not Miami."
**The Primary Call-to-Action (CTA):** Calling the front desk is the cultural norm and primary driver of business in Key West. Online automated booking (via Wix) is a secondary CTA for convenience, but the UI must heavily prioritize and reduce friction for making a phone call.
**The Content Strategy:** The site architecture must support generating highly specific, informative landing pages for every node in the 4-Level Ontology (Category -> Concern -> Procedure -> Product) to capture long-tail and local search intent, while prominently featuring specials/promotions.

## Memory 0fc4f0ac-5ebb-42e3-87db-0f1dfb734434
The current task involves replacing a Wix CMS site with an Astro static site. The steps include integrating Tailwind CSS, leveraging the design system outlined in DESIGN.md (unfound yet), and implementing dynamic routing from ontology.json. Additionally, SEO landing pages mapped hierarchically (Category > Concern > Procedure > Product) will be dynamically generated in Astro.

## Memory 41b5ad98-8085-4235-8f9f-a1e6b7c201cc
Tailwind CSS and Autoprefixer have been successfully initialized in the Astro project by adding configurations in postcss.config.cjs and tailwind.config.cjs. The necessary dependencies were also installed.

