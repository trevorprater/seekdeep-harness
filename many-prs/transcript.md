# Transcript — "i shipped 2000 PRs last month"

**Speaker:** lauren tan ([@poteto](https://x.com/poteto)) — Grok Bot @ SpaceXAI
**Talk:** recorded for Cursor Compile (London), released free on X
**Source:** https://x.com/poteto/status/2102050467505430555
**Runtime:** 38:02

Machine transcript (faster-whisper `distil-large-v3`), reflowed into paragraphs.
Recurring proper-noun misreadings were corrected from the slides (see
`raw/fixups.py`); everything else is unedited, so expect residual errors. `[slide NN]` markers show
where the canvas moved; the images are in `slides/`.

---



`[slide 01]` → `slides/slide-01_00m00s.jpg`

**[00:00:01]** Hi, my name's Lauren. You might know me as poteto, and I work on Grok Bot at SpaceX AI. So last month, I did something pretty crazy. I shipped 2,000 pull requests to production.

**[00:00:18]** A lot of how I'm able to do this is through trust, and I think a lot about trust in terms of how I can trust my agents to produce high-quality work, even when I'm not there. And my argument and thesis for this talk today is that if you set up your environment for your agents really, really well, you can end up with something that looks more like a personal or even team software factory, where you're producing very high quality code at much greater rates than before.


`[slide 02]` → `slides/slide-02_00m21s.jpg`


`[slide 03]` → `slides/slide-03_00m57s.jpg`

**[00:00:57]** But I'm not a fan of the term software factory. I like the analogy of a Michelin Kitchen better, where, you know, as technologists, we're not really producing, we're not mass-producing a product on an assembly line. But the work that we do looks very creative, it's very, you know, it's the act of building a product and it's art in some sense. So, you know, even though with agents, we're not cooking the individual components that go into the product anymore, we're still responsible for the final outcome and thinking about our kitchen setup, right? Because depending on how you set up, your line cooks, your sous chefs, you know, the kind of equipment they have, the kind of training they have, you know, dishwashers and the ratio, I guess, of line cooks to dishwashers, all of these ingredients go into making the final product.

**[00:01:57]** and I think this analogy is really apt. So I wanted to talk a little bit about a story before we begin, where six months ago, when I first joined Cursor, before we were a part of SpaceX AI, I obviously had, you know, no agent skills to use. I just joined the company. It was a fresh code base and a fresh product that I was working on. So at the time, Cursor was building the replacement for the Cursor IDE, which is the new agent's window. And before I joined, the Cursor agent window had quite a lot of performance issues. And my manager at the time asked if I was able to help them. And since I had spent some time on the React team before I joined Cursor, it seemed like a good fit.


`[slide 04]` → `slides/slide-04_02m09s.jpg`

**[00:02:56]** But when I first started, I quickly realized how manual this process was. Obviously, I have done performance work before, but at the rate at which pull requests were being landed, it was just, it felt almost an insurmountable wall of pull requests that just kept coming in, and I had no idea whether or not the performance of the app would be regressing, right? So a lot of my early time on the cursor team was spent looking at Chrome DevTools and doing performance traces and taking heap snapshots. And it was extremely, extremely manual.

**[00:03:41]** It was so manual. It got to the point where I just got really frustrated and started to think about, you know, wait, we have agents. What am I doing? And so I started thinking about verification skills, where, you know, what if my agent could actually run the application itself and take the traces for me, understand the traces and find the hot spots, and basically hill climb, you know, to a better performance in our application automatically.

**[00:04:14]** And throughout the last six months of being a cursor and SpaceX AI, you can really see that my product, it really has skyrocketed. And, you know, I never set out to ship 2,000 pull requests a month. That was not a goal of mine at all. But I realized that all of the skills, all of the tools and code-based changes I were making laddered up to this idea of trust. You know, I didn't know it at the time, but I had this thought in the back of my head, which was, you know, I am the bottleneck and I need to be able to take all of the knowledge that I have as an engineer and impart them into my team of agents so that I didn't need to be the blocker for everything.

**[00:05:04]** And you can clearly see that it's paid off.

**[00:05:11]** So I think it really comes down to trust, but how exactly do you build that trust? and where do you start if you are, you know, wherever you are in your journey of using agents? So when I started, obviously, I was in this category, you know, in the one to one to five range, where you still feel like you have to babysit every chat and conversation, and you're just constantly course correcting, you know, you're intervening, you're correcting your agent so that it does the right thing. And if you're not there, basically nothing happens, and the agents do the wrong thing.


`[slide 05]` → `slides/slide-05_05m12s.jpg`

**[00:05:59]** And I would actually argue that this phase of, you know, using agents is actually the hardest to get out of, because it's not always very clear how exactly you get out of it. And again, it just comes down to trust. You know, the reason you're unable to go from, one or one to five agents to something more like a hundred is because you don't have trust in your agents work yet. So if you don't have trust and you try to spawn a hundred sub-agents or cloud agents, you're going to quickly find that you're just going to get a ton of slop pull requests and, you know, a bunch of regressions and a bunch of bugs ship and no one's going to be very happy with them. So that begs to question, how do you trust your agents more?

**[00:06:49]** So for me, it came, it really started when I, like I said, I meant I joined the cursor team, and I was starting to work on performance, where I realized the need for verification. So when I say verification, there are sort of levels to that because, you know, on the, I guess, the lower end of the scale for verification, you have things like the verification. skills that I talk about where you teach your agent how to run your application and use things like the Chrome DevTools protocol or whatever other protocol that you have for debugging and teach them how to you know debug the application take performance traces


`[slide 06]` → `slides/slide-06_06m51s.jpg`


`[slide 07]` → `slides/slide-07_07m09s.jpg`


`[slide 08]` → `slides/slide-08_07m24s.jpg`

**[00:07:41]** take heap snapshots and so on and then on the opposite end of spectrum of the spectrum, which is much, much harder and still very much an open question, is like more formal verification, where, you know, maybe you rely on formal methods or, you know, languages like lean or TLA+ to do, you know, verification so that you can check that, you know, your business level or business logic invariants are, you know, are always true. and that you can formally verify that your application is always in a correct state. But I would say that even if you don't have the ability to run formal methods and very few people really do, that with verification skills you can get very far.

**[00:08:36]** So when I joined the cursor and started to work on the cursor agent window, the first skill that I built was the skill called control glass, which is a verification skill that teaches the agent how to run the application and take traces, like I mentioned, and it does that through the Chrome DevTools Protocol.

**[00:09:01]** Something interesting about that skill is, and I kind of iterated my way to this, it didn't start out this way, but the control or verification skill really has two components to it. The first part is obviously a CLI, so you want your agents to be able to reproducibly be able to run the application and collect traces and collect evidence that, you know, empirical evidence, that, you know, code is working, that your performance bar is being met, and rather than have your agents create scripts every time, you know, that can differ between agents. agent sessions, you can actually create a CLI that's within the skill directory, and then your agents will just use that every time. And then, of course, you need to actually


`[slide 09]` → `slides/slide-09_09m09s.jpg`

**[00:10:00]** invest in it and make it good so that it can handle all sorts of different use cases and be able to, you know, run the application correctly. Another thing that's also really important is this idea of a feature map. So a feature map really is something that I kind of coined, I guess, where, you know, we started using these control skills within cursor. Then we quickly realized that, you know, a Slack report would come in and a user would post a very vague screenshot, right? Like a very small slice of the UI and just like three question marks. And the agents using our control skills are just no idea. Like it could run the application, but it was just guessing at what exactly the user meant. And so I had this idea to create something called a feature map, which is, I guess, kind of inspired by like a site map.

**[00:11:02]** It essentially is a form of materialized memory. You know, like how exactly does your application work? What features does it have, how does a user reach it, you know, in terms of keyboard shortcuts or what DOM elements to click on, you know, things like that, and what all the different features do.

**[00:11:26]** And they are these, this feature map is stored in the skill itself in the code base as part of the skill directory. And we have an automation that maintains this feature map as well. But when we combine the CLI and the feature map, we quickly realized that this combination was very, very powerful because now agents could not only, you know, reproducibly control the application and take traces, but it could also understand requests that came in from internal users as well as external users. And so we very quickly realized that the control verification skills were so useful that they've become more or less critical infrastructure for our team, and we constantly maintain it.

**[00:12:18]** But the ability for an agent to verify its own work is extremely powerful, and we have spent a lot of time on this skill, and it's very, very powerful for building that trust. trust. In addition to verification, of course, you want, because verification is really about correctness. Correctness to me is really about, does the thing, that's the feature or code do the thing that you want it to do, right? Like, does the, you know, the checkout button, does it actually check out the card? Verification is really important for that because you can get empirical evidence that this feature actually works. But it doesn't tell you much about, you know, the performance or the code quality of that feature. And that's where you start thinking


`[slide 10]` → `slides/slide-10_12m33s.jpg`

**[00:13:13]** about skills that teach agents to work like real software engineers. So I've built a plugin called pstack. I'm not going to talk about the plugin too much today, but a lot of inspiration for that plugin, which is a collection of skills that I've created, are inspired by the kind of workflows that I personally have used in my time doing software engineering for all sorts of different types of tasks. So debugging, you know, feature development, prototyping, there's a whole bunch of different playbooks and skills that ship in pstack that teach your agents how to write code the way that you want them to. And, you know, this is where, you know, the more experienced engineers on your team can really

**[00:14:07]** contribute to set up a team repository of skills that just make your agents a lot smarter.

**[00:14:17]** And when you combine those skills with verification skills, then you're able to get the point where your agents are able to not just verify that the work that they're doing is correct. but that is also high quality. And again, because of verification, you can collect real performance metrics, you can get real numbers and statistics and telemetry on the performance of the application.

**[00:14:49]** So I think that's a really important part to invest in.

**[00:14:55]** Another thing I think is really important is refactoring and rewriting your architector to be more agent-friendly.


`[slide 11]` → `slides/slide-11_15m03s.jpg`

**[00:15:04]** And I almost want to say that this is one of the most important things you can do as a software engineering team, because if you really truly believe that agents are going to be writing all the code in the future, then we need to design our code bases so that they do the right thing by default. And you'll find that there's this, I almost want to say like a, a scale or continuum between, you know, these different pieces of building trust in your agents, where on, you know, I have like five of these points here, where, uh,


`[slide 12]` → `slides/slide-12_15m39s.jpg`

**[00:15:45]** the code base is really like the best form of memory because agents love to extend existing patterns that they see. Um, and, you know, uh, I think this is just the nature of how LLMs work. where they're more likely to use whatever it is in their context window to make changes. And of course the files that the agents read and opens are part of its context window, and therefore the code base is a really important part of that. Because, you know, agents aren't going to just refactor your code in every single PR. They're going to just look at what's already there and just extend. The next level I think is about static analysis, where you have linters, you have compiler diagnostics, you have continuous integration, and these are guidelines and constraints that you can enforce in your code base so that whenever you correct your agent, you find that, you know, they just keep making the same mistake.

**[00:16:53]** you can add those as lint rules or even better you can refactor your code base so that the mistake that the agent is making becomes categorically impossible and then a step above that right where and this is where we're trying to get more into less of like a hard constraint and enforcement and more into the realm of guidance you have things like rules you have Bugbot you have skills which you know, your agents will obviously sometimes and mostly use when they're doing their work, but there is also a chance that it might, for various reasons, you know, forget to read a rule or maybe the user that is piloting the agent, ignores them. So these aren't quite as enforceable, but also an important part of setting up your environment

**[00:17:48]** so that you can really trust what your agents are doing.

**[00:17:53]** And then finally you have a style guide, which is really only enforceable by humans in a code review. I guess you could put these in your rules and Bugbot and skills as well. But if you don't, then you have this big glaring hole in your review process, where now humans have to, you know, look at every single line that's being changed and remember the comment. And with the rate of pull requests that are coming in, it just becomes impossible. So I definitely wouldn't recommend, you know, relying only on the style guide. I think the style guide or, you know, like looking at human reviews, is a good place to start in terms of what's missing. But you should really invest the time to think about the other four parts, you know, your code base, making things categorically impossible through better data structures or algorithms, static analysis, and then, of course, you layer that with rules.

**[00:18:53]** and Bugbot and skills.

**[00:18:57]** On the code base front,

**[00:19:01]** and in the Grok Bot code base, we actually have invested into setting something up that we call Dune, which is our agent-friendly framework. So the inspiration for Dune really came about from a lot of the performance issues we were seeing in the cursor agent window. And so a lot of lessons came out of that exploration, but the key principle that we landed on is really that agents love taking shortcuts. So what if we designed a framework such that the shortcut, you know, the easy path is the right path for agents. And also one that would be a code base that is, you know, maybe pretty annoying for humans to work in because it's so locked down in terms of what you


`[slide 13]` → `slides/slide-13_19m03s.jpg`

**[00:19:54]** can do and what you can't do. But it actually creates the perfect environment for agents, especially ones that have very minimal context because, you know, not every contributor to your codebase is going to be an engineer anymore. You can have designers, you can have product managers, you can have CEOs, you know, going into the codebase and shipping features. So we want to really think a lot about how we invest and set up our codebases so that, you know, even agents that are piloted by busy people with not a lot of context can do a good job by default.

**[00:20:31]** And like I mentioned before, your codebase is really a form of memory for agents because they love to extend the existing patterns that they see. And the reverse is actually also true, right? You can invest a time to set up your code base in a way that things are, you know, bad patterns are categorically impossible. or you have lint rules that prevent them but the reverse is also true in the sense that if you have existing anti-patterns you'll actually find that these will spread kind of like a virus where you have like one small workaround or a comment that explains a workaround and you'll quickly find that agents just love to copy that and then in a matter of a few days or a few weeks

**[00:21:23]** you'll find that the workaround has spread everywhere and it's now becomes, it has become like a de facto pattern for all agents. And that's a really, really bad place to be in. And that goes back to what I was saying about why it's really important, you know, whenever you're correcting your agents, that you invest the time into thinking about code-based changes and static analysis and, of course, layering them with rules, good rules, and Bugbot and skills because of that reason and another analogy that I like in addition to the Michelin Kitchen is this idea that you know your codebase is kind of like a garden where you have workarounds you know that seem seemingly that seem kind of innocent at first but then because


`[slide 14]` → `slides/slide-14_21m42s.jpg`


`[slide 15]` → `slides/slide-15_22m00s.jpg`


`[slide 16]` → `slides/slide-16_22m12s.jpg`

**[00:22:16]** of the nature of agents you just copy that pattern over and over again and you quickly end up with a very, you know, vibe-coded codebase that is, you know, a pain in the butt to maintain and has a lot of performance issues. So in my opinion, the perfect agent code base is one that's so locked down that, you know, it's, again, it's like really annoying for humans to write code in, but it's so conventional, it's so standardized that, you know, even innocent-looking patterns are just forbidden. And the best example of this I have is actually something that seems very, very innocent when you look at it, but when you think about it, it's actually really bad. And that pattern is agent leaving comments in the code. Now, you know, when I first


`[slide 17]` → `slides/slide-17_22m24s.jpg`

**[00:23:10]** saw agents starting to do this, I initially wasn't, well, I did think that a lot of them slop, but I also thought that, you know, it actually doesn't, it's not a bad thing, right, I guess, if the agents are leaving comments in the code, because as humans, we left comments in the code whenever we saw, you know, edge cases, or we needed to actually make a workaround or, you know, leave a note to ourselves or a colleague on a particularly tricky part of the code base. But what I quickly realized when we saw this happening in the cursor codebase, was that agents were just using the comments around the code as justification for why it wasn't going to solve the actual problem

**[00:23:56]** and instead paper over it with a band-aid or a short-term solution.

**[00:24:03]** So in Dune, which is, again, the framework that powers Grok Bot, we made the choice to we made the choice to actually ban comments for that reason so that agents would not just copy that pattern and propagate it everywhere in the codebase so my pitch here is that every team really needs something that a role that I'm calling a gardener in the same way that you know with a real garden you need someone who is thinking a lot about the, you know, things that can kind of creep in and grow in ways that you don't want. Like, you know, you have weeds, you have, you know, just other types of organic growth. I don't really know gardening that well. But you have things that, you know, unwanted pests and stuff like that that kind of creep in


`[slide 18]` → `slides/slide-18_24m30s.jpg`

**[00:25:05]** empirical based. And so you want to nip them in the bud as soon as possible before they start propagating everywhere. A lot of the principles behind Dune are really centered around these three things. First of all, we want to delete tech debt that we already have for, you know, for reasons I just mentioned. We want to keep or enforce a single paved path for most blessed patterns. You know, there should be one conventional way to do some things so that agents don't really need to guess and there should be enough guidance in the code base, in CI, in lint rules, so that the agents are guided to follow that path.

**[00:25:52]** And then finally, whenever you see tech-dap or bad patterns, your instinct should be, I need to write a lint rule against it. You don't always have to clean it up immediately because if you write a lint rule, you can at least stop the bleeding, and which doesn't stop. the problem entirely, but it at least prevents it from growing. So I definitely recommend, you know, really thinking a lot about how you can guard against anti-patterns so that they don't spread like a virus. And then also spend time to actually, you know, get your agents to clean them up so that your code base is just constantly kept in a state where you would be happy if an agent were to copy.

**[00:26:37]** that's the kind of mindset that I would recommend having.

**[00:26:42]** And then I won't actually go through all the details of Dune itself, but I'll just kind of gloss through some interesting parts. So again, as a reminder, doing this the architecture, the client framework that we built to power Grok Bot.


`[slide 19]` → `slides/slide-19_26m45s.jpg`

**[00:26:56]** We've invested a lot into all the things I was saying, where we have conventions. We have a lot of conventions about where code should live and where and how code should be imported between them. So in Dune applications, you know, there's different concepts where, like, for example, features are all co-located in a single folder. You have an entry point that's, you know, in the React part of the code that determines, you can kind of think of it like a route. You have transcript cards that show up in the Grok Bot application. You have a host that runs on the, you know, the Grok Bot virtual machine, and then, of course, you have your client, which powers the overall Dune application.


`[slide 20]` → `slides/slide-20_27m03s.jpg`

**[00:27:44]** And we have a lot of strict boundaries between these things, where, just as an example, things that run on the main process or the main thread in electron aren't allowed to be run on the renderer thread. and we keep that separation very intentionally because of lessons we learned from cursor's agent window, where we would sometimes see code accidentally get imported into the renderer thread and, you know, slow code. And since on the render thread you want your UI to be very smooth and performant, you need to make sure that you don't have any long tasks or, you know, things that take long, longer than 16 milliseconds, or if you want like 60 frames per second, or 8 milliseconds if you want 120 frames per second.


`[slide 21]` → `slides/slide-21_27m48s.jpg`

**[00:28:39]** And so your renderer has to be constantly in a state where it is, you can really kind of chunk up the work and not do them all at once. And so we have code within Dune that enforces these boundaries through the import and dependency graph. But yeah, it's just an example of a pattern that we saw lead to really bad performance that we categorically eliminated through the architecture of the of Dune.

**[00:29:17]** And then all of these other pieces aren't that interesting, but again, the core theme here, you know, it's not about Dune, but the idea that an agent-friendly framework of your own is actually very, very powerful, and you can encode all of the learnings that you and your best engineers on your team have tribal knowledge of, and I think the lesson here is that how do you take that away from, you know, what used to be in the style guide process of reviewing code and, you know, engineers reviewing other engineers work and leaving comments to extract almost like extracting that knowledge and encoding that into the framework into the codebase itself so that the codebase access the memory


`[slide 22]` → `slides/slide-22_29m27s.jpg`

**[00:30:12]** right it's the the thing I keep coming back to this idea that you know the codebase is just the thing that it's the the materialized snapshot of the state in which you want your agents to extend and you want that code base to be so pristine so great that that the next agent that comes along is just very likely to continue that pattern and keep it really, really good.

**[00:30:38]** And if you spend enough time on this process, like I mentioned, you can really set up a Michelin Kitchen or a software factory, where because you've spent so much time on, you know, all of these pieces that allow you to trust your agents, whether it's in the code base, whether it's lint rules, whether it is, you know, diagnostics or rules or Bugbot or skills, these layers come together and provide you a lot of trust. Because now, you know, just imagine for a moment you're working in the Grok Bot code base.


`[slide 23]` → `slides/slide-23_30m48s.jpg`

**[00:31:20]** It's super locked down, you know, it's like almost impossible to write bad code. so you know you can even an agent with very little context you know even an agent with not a lot of reasoning can come in and actually write code that's good and going back to my example about the michelin factory i think there's a lot here right where you know we're setting up our agents our bots with skills and tools you know we're training them we're sitting up our kitchen in a way that makes sense, right? For the agents and bots to do the right thing by default. You know, whenever we see, for example, in the kitchen example, if we notice that one of our cooks or

**[00:32:05]** dishwashers is constantly tripping over something, of course we need to fix that, right? We need to problem solve and ensure that, you know, others don't trip as well because, you know, in a kitchen is a very dangerous place and you don't want to hurt yourself. It's the same mindset I think that we should have with our codebases, how do we set it up so that even agents without a lot of knowledge can do a good job? And I think with, you know, Grok Bot, Grok Bot and cursor play an interesting role together, where Grok Bot is really great at providing what I call the outer loop because you can connect Grok Bot to to lots of different connectors like Slack to Datadog, Sentry, PlanetScale, whatever services that you use.


`[slide 24]` → `slides/slide-24_32m54s.jpg`

**[00:32:59]** And you can aggregate all of that information together and use that to make really good decisions for itself.

**[00:33:07]** Some people call this like a company brain. I don't really think, I personally don't think you need anything that sophisticated here because agents are really good at using tools. and so if you connect these tools to Grok Bot and you start having your Grok Bot's auto kickoff things like cloud agents you can actually find that it's really not you don't really have to invest in a lot of infrastructure to build a software factory in fact I'm going to you know cross cross out this this term because I don't like this term I think you can set up this personal Michelin Kitchen for yourself through Grok Bot things like Grok Bot routines which let you subscribe to

**[00:33:54]** Slack threads to Sentry alerts that let you kick off things automatically and when you combine all of these things that I've been mentioning, you know, your code base your rules, your skills they all compound and Grok Bot will be able to you know automatically respond to events that come from the outer loop and then kick off cloud agents. And you can also set up cursor automations and use our SDK to set up additional bots as well that reuse a lot of these pieces of agent infra that you've set up and allow them to do much more complicated tasks. So if you've done all this, then I think you can get to a point where, you know, I have some screenshots here of some of our automations and our agents that work on cursor,


`[slide 25]` → `slides/slide-25_34m48s.jpg`

**[00:34:56]** where we are automatically reproducing bug reports, we're automatically opening pull requests, we are essentially adding a lot of value to the entire team because all of these things compound. So if we kind of zoom out again and go back to this graph, I think that to kind of close off the talk, if you spend a lot of time thinking about all of the pieces that you need to be able to ascend the trust graph, you start getting to a place where you can really trust your agents more and parallelize your work and also empower your entire team to build on top of of these pieces of infrastructure for your agents and empower everyone, you know, every engineer


`[slide 26]` → `slides/slide-26_35m03s.jpg`


`[slide 27]` → `slides/slide-27_35m24s.jpg`

**[00:35:54]** on your team, every builder to be extremely productive and be able to write high quality code. So the last thing I want to leave you with is actually this piece.

**[00:36:06]** Sorry, not that piece, but this piece. I think if there's only one thing you take away from my talk, it should be this slide here, which is, you know, these are the These are the activities that will help you build up towards a high trust environment. Whenever you find yourself correcting and intervening your agent, you really want to think about it from these five pieces. And where is the most effective step in this sequence in order to make your agent much more trustworthy? And of course, I definitely recommend thinking about it in this order, where, you know, you either invest the time to make that pattern categorically impossible through your code base and


`[slide 28]` → `slides/slide-28_36m09s.jpg`

**[00:36:58]** architecture and data structures, or you start looking at things like static analysis, and then you layer that on with rules and Bugbot and skills. If you do all of that, and you also spend some time, you know, thinking about your code quality, in terms of skills, you get to a place where you trust the environment so much that your agents can just be free. Right. And personally, I have spent a lot of time for this for Grok Bot's codebase, for example, and this is really the secret. Well, it's not really a secret. It's a lot of hard work, but I hope you found this talk useful. And please reach out to me on X. My handle is poteto with an e.

**[00:37:51]** And I hope that you'll have a lot of fun and success in your own Michelin Kitchen. Thanks for watching.
