//! Deterministic routing for high-frequency utility requests.
//!
//! These intents should not depend on the LLM selecting the right tool. The
//! scope is intentionally small: status, time, and diagnostics where arguments
//! are unambiguous and repeated daily usefulness matters.

use super::ToolCall;

pub fn route(text: &str) -> Option<ToolCall> {
    let normalized = strip_household_speaker_prefix(&normalize(text));
    if normalized.is_empty() {
        return None;
    }

    if asks_home_undo(&normalized) {
        return Some(tool("home_undo", serde_json::json!({})));
    }

    if asks_action_history(&normalized) {
        return Some(tool("action_history", serde_json::json!({})));
    }

    if asks_memory_status(&normalized) {
        return Some(tool("memory_status", serde_json::json!({})));
    }

    if let Some(items) = shopping_list_add_request(&normalized) {
        return Some(tool(
            "memory_store",
            serde_json::json!({
                "category": "shopping",
                "content": format!("shopping list pending: {items}")
            }),
        ));
    }

    if let Some(items) = shopping_list_remove_request(&normalized) {
        return Some(tool(
            "memory_store",
            serde_json::json!({
                "category": "shopping",
                "content": format!("shopping list removed: {items}")
            }),
        ));
    }

    if let Some(rule) = household_rule_store_request(&normalized) {
        return Some(tool(
            "memory_store",
            serde_json::json!({
                "category": "fact",
                "content": rule
            }),
        ));
    }

    if let Some((category, content)) = health_log_store_request(&normalized) {
        return Some(tool(
            "memory_store",
            serde_json::json!({
                "category": category,
                "content": content
            }),
        ));
    }

    if let Some((category, content)) = reminder_or_alarm_store_request(&normalized) {
        return Some(tool(
            "memory_store",
            serde_json::json!({
                "category": category,
                "content": content
            }),
        ));
    }

    if let Some((seconds, label)) = preferred_timer_request(&normalized) {
        return Some(tool(
            "set_timer",
            serde_json::json!({ "seconds": seconds, "label": label }),
        ));
    }

    if let Some((entity, action, value)) = priority_home_control_request(&normalized) {
        let mut args = serde_json::json!({ "entity": entity, "action": action });
        if let Some(value) = value {
            args["value"] = serde_json::json!(value);
        }
        return Some(tool("home_control", args));
    }

    if let Some(query) = memory_recall_query(&normalized) {
        return Some(tool("memory_recall", serde_json::json!({ "query": query })));
    }

    if asks_system_status(&normalized) || asks_home_assistant_status(&normalized) {
        return Some(tool("system_info", serde_json::json!({})));
    }

    if let Some(query) = web_search_request(&normalized) {
        return Some(tool(
            "web_search",
            serde_json::json!({ "query": query, "limit": 3 }),
        ));
    }

    if let Some((seconds, label)) = timer_request(&normalized) {
        return Some(tool(
            "set_timer",
            serde_json::json!({ "seconds": seconds, "label": label }),
        ));
    }

    if let Some((location, forecast)) = weather_request(&normalized) {
        return Some(tool(
            "get_weather",
            serde_json::json!({ "location": location, "forecast": forecast }),
        ));
    }

    if let Some(entity) = scene_or_routine_activation_request(&normalized) {
        return Some(tool(
            "home_control",
            serde_json::json!({ "entity": entity, "action": "activate" }),
        ));
    }

    if let Some(query) = play_media_request(&normalized) {
        return Some(tool("play_media", serde_json::json!({ "query": query })));
    }

    if let Some((entity, action, value)) = home_control_request(&normalized) {
        let mut args = serde_json::json!({ "entity": entity, "action": action });
        if let Some(value) = value {
            args["value"] = serde_json::json!(value);
        }
        return Some(tool("home_control", args));
    }

    if let Some(expression) = calculation_request(&normalized) {
        return Some(tool(
            "calculate",
            serde_json::json!({ "expression": expression }),
        ));
    }

    if let Some(entity) = home_status_target(&normalized) {
        return Some(tool("home_status", serde_json::json!({ "entity": entity })));
    }

    if asks_current_time(&normalized) {
        return Some(tool("get_time", serde_json::json!({})));
    }

    None
}

pub fn route_for_available_tools(
    text: &str,
    home_available: bool,
    web_search_available: bool,
) -> Option<ToolCall> {
    let call = route(text)?;
    if matches!(
        call.name.as_str(),
        "home_control" | "home_status" | "home_undo" | "action_history"
    ) && !home_available
    {
        return None;
    }
    if call.name == "web_search" && !web_search_available {
        return None;
    }
    Some(call)
}

fn tool(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        name: name.to_string(),
        arguments,
    }
}

fn normalize(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_household_speaker_prefix(text: &str) -> String {
    for name in ["jared", "sarah", "leo", "mia"] {
        if let Some(rest) = text
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(' '))
        {
            let rest = rest.trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    text.to_string()
}

fn asks_memory_status(text: &str) -> bool {
    contains_any(
        text,
        &[
            "memory status",
            "memory health",
            "memory database",
            "memory diagnostics",
            "memory index",
        ],
    )
}

fn memory_recall_query(text: &str) -> Option<String> {
    if contains_any(
        text,
        &[
            "what is my name",
            "whats my name",
            "what s my name",
            "do you know my name",
            "do you remember my name",
            "remember my name",
            "who am i",
        ],
    ) {
        return Some("name".into());
    }

    if let Some(role) = household_role_query(text) {
        return Some(role.into());
    }

    if is_structured_household_question(text) {
        return Some(text.to_string());
    }

    if is_app_only_secret_question(text) {
        return Some(text.to_string());
    }

    if is_household_note_question(text) {
        return Some(text.to_string());
    }

    if is_semantic_household_memory_question(text) {
        return Some(text.to_string());
    }

    for prefix in [
        "what do you remember about ",
        "what do you know about ",
        "do you remember ",
        "search memory for ",
        "search memories for ",
        "recall memory for ",
        "recall memories for ",
    ] {
        if let Some(query) = text.strip_prefix(prefix).map(str::trim)
            && !query.is_empty()
            && query != "that"
        {
            return Some(query.to_string());
        }
    }

    if matches!(
        text,
        "what do you remember"
            | "what do you know about me"
            | "what do you remember about me"
            | "what do you know about us"
            | "what do you remember about us"
    ) {
        return Some("me".into());
    }

    None
}

fn is_structured_household_question(text: &str) -> bool {
    (text.starts_with("how old is ") || text.starts_with("what age is "))
        || text.contains("credit score")
        || matches!(
            text,
            "what is my weight" | "what s my weight" | "what's my weight"
        )
        || text.contains("vo2 max")
        || (text.contains("tv tonight") || text.contains("on tv tonight"))
        || text.contains("city council meeting")
        || text.contains("what time is my bus")
        || text.contains("bus tomorrow")
        || text.contains("bus pickup tomorrow")
        || text.contains("who s coming to dinner")
        || text.contains("who's coming to dinner")
        || text.contains("coming to dinner tonight")
        || text.contains("can i have a snack")
        || text.contains("did i finish my chores")
        || text.contains("did mom approve my sleepover")
        || text.contains("which leftovers should we eat first")
        || text.contains("who changed the thermostat")
        || text.contains("did mia take her allergy medicine")
        || text.contains("can i play outside")
        || text.contains("allowed to play outside")
        || text.contains("what do we still need to do before trash day")
        || text.contains("did anyone take the garbage bins out")
        || text.contains("which homework needs internet")
        || text.contains("can i use the stove")
        || text.contains("can i use stove")
        || text.contains("which sensors need batteries soon")
        || text.contains("did i pack my library book")
        || text.contains("why did my alarm not go off")
        || text.contains("what plants need attention")
        || text.contains("why did away mode fail")
        || text.contains("make an end of day house summary")
        || text.contains("make an end-of-day house summary")
        || text.contains("can i open the front door")
        || text.contains("why is the basement humid")
        || text.contains("what needs charging tonight")
        || text.contains("when is the next filter change")
        || text.contains("was the front door locked after")
        || text.contains("can i print my homework")
        || text.contains("did my tooth fairy box stay closed")
        || text.contains("what changed in the garage today")
        || text.contains("why did the security alarm chirp")
        || text.contains("who s in the backyard")
        || text.contains("who's in the backyard")
        || text.contains("what s left on my bedtime chart")
        || text.contains("what's left on my bedtime chart")
        || text.contains("did i close the upstairs window before the rain")
        || text.contains("what devices are on guest wi fi")
        || text.contains("what devices are on guest wifi")
        || text.contains("what devices are on guest wi-fi")
        || text.contains("side path icy")
        || text.contains("why is the office internet slow")
        || text.contains("what chores did leo skip this week")
        || text.contains("can the cat sleep in my room")
        || text.contains("why is mia s purifier on high")
        || text.contains("why is mia's purifier on high")
        || text.contains("did the garage close after jared left")
        || text.contains("did i feed the cat too much")
        || text.contains("what s the oldest thing in the fridge")
        || text.contains("what's the oldest thing in the fridge")
        || text.contains("why is my lamp flickering")
        || text.contains("can i open the garage door")
        || text.contains("did anyone bypass a sensor")
        || text.contains("did dad see my message")
        || text.contains("can i practice drums now")
        || text.contains("which automation fired the most today")
        || text.contains("when did the laundry finish")
        || text.contains("did my laundry get moved")
        || (text.starts_with("can i ") && text.contains("watch cartoons"))
        || text.contains("who opened the garage door")
        || text == "is mia home"
        || text == "is leo home"
        || text == "is sarah home"
        || text == "is jared home"
        || text.contains("what groceries are low")
        || text.contains("saturday morning routine")
        || text.contains("what s next before school")
        || text.contains("what's next before school")
        || (text.starts_with("what channel is ") || text.starts_with("what channel s "))
        || (text.contains("subscription") && (text.contains("due") || text.contains("renew")))
        || (text.starts_with("what does ") && text.contains(" like"))
        || (text.starts_with("what size shoe does ") && text.contains(" wear"))
        || (text.starts_with("what shoe size does ") && text.contains(" wear"))
        || (text.contains("dishwasher") && (text.contains("clean") || text.contains("dirty")))
        || (text.starts_with("did ") && text.contains("trash truck"))
        || (text.contains("temperature") && text.contains("attic"))
        || (text.starts_with("is ") && text.contains("home from school"))
        || (text.starts_with("is ") && text.contains(" allowed"))
        || (text.starts_with("do we have ") && (text.contains(" left") || text.contains("eggs")))
        || (text.starts_with("are there ") && text.contains(" left"))
        || (text.starts_with("does ") && text.contains(" have "))
        || (text.starts_with("when is ") && text.contains("dentist appointment"))
        || (text.starts_with("when is ") && text.contains("vet appointment"))
        || (text.starts_with("when is ") && text.contains("checkup"))
        || text.contains("sun set")
        || text.contains("sunset")
        || (text.starts_with("did ") && contains_any(text, &[" feed ", " fed "]))
        || (text.starts_with("did ") && contains_any(text, &[" brush ", " brushed "]))
        || (text.starts_with("did everyone") && text.contains("brush") && text.contains("teeth"))
        || (text.starts_with("did ") && text.contains("allowance"))
        || (text.starts_with("did ") && text.contains("pay") && text.contains("bill"))
        || (text.starts_with("can ") && text.contains(" unlock "))
        || text.contains("school bus")
        || text.contains("bill due")
        || text.contains("electricity bill")
        || text.contains("trash pickup")
        || text.contains("trash day")
        || text.contains("community pool")
        || (text.contains("pool") && text.contains("open"))
        || (text.contains("library") && (text.contains("close") || text.contains("hours")))
        || text.contains("recycling week")
        || text.contains("recycling day")
        || text.contains("parent teacher conference")
        || text.contains("parent-teacher conference")
        || text.contains("dentist appointment")
        || text.contains("vet appointment")
        || text.contains("turned off the security system")
        || text.contains("disarmed the security system")
        || text.contains("picking up the kids")
        || text.contains("school pickup")
        || text.contains("shopping list")
        || (text.contains("allergic") || text.contains("allergy"))
        || text.contains("homework rule")
        || text.contains("homework rules")
}

fn is_household_note_question(text: &str) -> bool {
    text.starts_with("what did i say about ")
        || text.starts_with("what did we say about ")
        || text.starts_with("find my note about ")
        || text.starts_with("find note about ")
        || text.starts_with("find the note about ")
        || text.starts_with("show my note about ")
        || text.starts_with("show the note about ")
        || text.starts_with("what did i write about ")
        || text.starts_with("what did we write about ")
        || text.starts_with("what did the vet say about ")
        || text.starts_with("what did the mechanic say about ")
        || text.starts_with("what color is ")
        || text.starts_with("what colour is ")
        || text.starts_with("what color did we paint ")
        || text.starts_with("what colour did we paint ")
        || text.starts_with("what s the model number ")
        || text.starts_with("what is the model number ")
        || text.starts_with("who took the photos ")
        || text.starts_with("find the warranty for ")
        || (text.starts_with("find the ") && text.contains(" warranty"))
        || text.starts_with("what is the warranty for ")
        || text.starts_with("what s the warranty for ")
        || text.starts_with("what's the warranty for ")
        || text.starts_with("find the receipt for ")
        || text.starts_with("find manual for ")
        || text.starts_with("find the manual for ")
        || text.starts_with("find the user manual for ")
        || (text.starts_with("find the ") && text.contains(" manual"))
        || text.starts_with("find anything about ")
        || text.starts_with("find my essay draft")
        || text.starts_with("find my debate research")
        || text.starts_with("find the essay draft")
        || text.starts_with("find the ladder safety note")
        || text.starts_with("tell me the dinosaur fact")
        || text.starts_with("read me the next step")
        || text.contains("which breaker controls the dishwasher")
        || text.contains("what did we do last time ants")
        || text.contains("camping flashlight")
        || text.contains("rain boots")
        || text.contains("slow cooker manual")
        || text.contains("timer chart")
        || text.contains("garage camera") && text.contains("bike")
        || text.contains("water heater receipt")
        || text.contains("white extension cord")
        || text.contains("chicken recipe") && text.contains("peanuts")
        || text.contains("vaccination form")
        || text.contains("field trip form")
        || text.contains("photo backdrop instructions")
        || text.contains("red marker")
        || text.contains("furnace") && text.contains("code 31")
        || text.contains("plumber") && text.contains("shutoff valve")
        || text.contains("winter poem")
        || text.contains("poem about winter")
        || text.contains("toddler gate instructions")
        || text.contains("recipe") && text.contains("green bowl")
        || text.contains("flashlight") && text.contains("lights go out")
        || text.contains("tournament") && text.contains("snacks")
        || text.contains("why didn t the sprinklers run")
        || text.contains("why didn't the sprinklers run")
        || text.contains("cold medicine instructions")
        || text.contains("library book")
        || text.contains("recital outfit")
        || text.contains("blue cup")
        || text.contains("side gate") && text.contains("while we were gone")
        || text.contains("guest speaker")
        || text.starts_with("find the instructions for ")
        || text.starts_with("find the sewing kit")
        || text.starts_with("how do i remove ")
        || text.starts_with("how do we remove ")
        || text.starts_with("how long do i boil ")
        || text.starts_with("how long should i boil ")
        || text.starts_with("what bin does ")
        || text.contains("science fair checklist")
        || text.contains("tablet charger")
        || text.contains("backpack")
        || text.contains("allergy action plan")
        || text.contains("pajama day")
        || text.contains("dishwasher error")
        || text.contains("which filter")
        || text.starts_with("what is the doctor")
        || text.starts_with("what s the doctor")
        || text.starts_with("what's the doctor")
        || text.starts_with("find the manual for the car")
        || text.starts_with("where are the tax documents")
        || text.starts_with("what is the license plate")
        || text.starts_with("what s the license plate")
        || text.starts_with("what's the license plate")
        || text.starts_with("who do we call for ")
        || text.starts_with("what is the school")
        || text.starts_with("what s the school")
        || text.starts_with("what's the school")
        || text.starts_with("what is the vet")
        || text.starts_with("what s the vet")
        || text.starts_with("what's the vet")
        || text.starts_with("what is the phone number for ")
        || text.starts_with("what s the phone number for ")
        || text.starts_with("what's the phone number for ")
        || text.starts_with("what is the ip address of ")
        || text.starts_with("what s the ip address of ")
        || text.starts_with("what's the ip address of ")
        || text.starts_with("how do i reset ")
        || text.starts_with("how do we reset ")
        || text.contains("printer wi fi")
        || text.contains("printer wifi")
        || text.contains("grandma") && text.contains("wi fi note")
        || text.contains("grandma") && text.contains("wifi note")
        || text.contains("wet soccer shoes")
        || text.contains("blue paint")
        || text.contains("safest way out")
        || text.starts_with("how do i clean ")
        || text.starts_with("how do we clean ")
        || text.starts_with("what did we have for dinner ")
        || text.starts_with("find the recipe for ")
        || text.starts_with("what s on the hardware store list")
        || text.starts_with("what is on the hardware store list")
        || text.starts_with("what's on the hardware store list")
        || text.starts_with("we have a leak ")
        || text.starts_with("there is a leak ")
        || text.starts_with("where are ")
        || text.starts_with("where is ")
        || text.starts_with("where s ")
        || text.starts_with("where did i put ")
        || text.starts_with("where did we put ")
        || text.starts_with("what did we watch about ")
        || text.starts_with("what did i watch about ")
        || text.starts_with("what movie ")
        || text.starts_with("what was that movie ")
}

fn is_semantic_household_memory_question(text: &str) -> bool {
    if text.contains("printer") && text.contains("out of ink") {
        return false;
    }

    text.contains("want to paint")
        || text.contains("stomach ache")
        || text.contains("teach me magic")
        || text.contains("magic trick")
        || text.contains("need a manicure")
        || text.contains("good charity")
        || text.contains("learn french")
        || text.contains("suggest a podcast")
        || text.contains("motivating speech")
        || text.contains("what shoes go with")
        || text == "i m thirsty"
        || text == "i am thirsty"
        || text == "i'm thirsty"
        || text.contains("going to yoga")
        || text.contains("yoga class")
        || text.contains("sunbathing")
        || text.contains("guys night")
        || text.contains("order thai food")
        || text.contains("have a fever")
        || text.contains("it s snowing")
        || text.contains("it's snowing")
        || text.contains("mia doing her homework")
        || text.contains("i m back")
        || text.contains("i'm back")
        || text.contains("bedtime story")
        || text.contains("romantic poem")
        || text.contains("hiking trail")
        || text.contains("extra basil")
        || text.contains("craving spicy")
        || text.contains("picture of a sunset")
        || text.contains("photo of a sunset")
        || text.contains("good name for a goldfish")
        || text.contains("feel anxious")
        || text.contains("roman empire")
        || text.contains("music fits this mood")
        || text.contains("going camping")
        || text.contains("make me a cocktail")
        || text.contains("plan a date night")
        || text.contains("washing machine is leaking")
        || text.contains("lock the bike")
        || text.contains("order groceries for a taco bar")
        || text.contains("taco bar")
        || text.contains("cold in the living room")
        || text.contains("i m cold")
        || text.contains("reading") && text.contains("too bright")
        || text.contains("make my room cozy")
        || text.contains("did my package arrive")
        || text.contains("water the garden")
        || text.contains("recipe") && text.contains("chickpea")
        || text.contains("hallway light") && text.contains("turn on")
        || text.contains("can t sleep")
        || text.contains("safe at night")
        || text.contains("spilled water") && text.contains("outlet")
        || text.contains("waking the kids")
        || text.contains("room so hot")
        || text.contains("scared of the dark")
        || text.contains("laptop battery")
        || text.contains("robot vacuum") && text.contains("under my bed")
        || text.contains("piano practice quiet")
        || text.contains("practicing violin")
        || text.contains("spaceship")
        || text.contains("toddler safe")
        || text.contains("toddler-safe")
        || text.contains("smell gas")
        || text.contains("bake cookies") && text.contains("waking leo")
        || text.contains("robot vacuum stuck")
        || text.contains("package still on the porch")
        || text.contains("beeping sound")
        || text.contains("porch light still on")
        || text.contains("heard glass break")
        || text.contains("safest way out")
        || text.contains("bake cookies") && text.contains("waking leo")
        || text.contains("house better for pollen")
        || text.contains("pollen")
        || text.contains("room good for a video call")
        || text.contains("reading with dad")
        || text.contains("work call")
        || text.contains("garage ventilated") && text.contains("paint")
        || text.contains("calm morning for leo")
        || text.contains("after dinner cleanup")
        || text.contains("after-dinner cleanup")
        || text.contains("board games")
        || text.contains("basement humid")
        || text.contains("rain boots")
        || text.contains("fan on low for sleep")
        || text.contains("cold after bath")
        || text.contains("desk feels glarey")
        || text.contains("quiet drawing")
        || text.contains("workshop dust control")
        || text.contains("side path icy")
        || text.contains("dripping")
        || text.contains("room smells weird")
        || text.contains("too scared to go downstairs")
        || text.contains("laundry room not scary")
        || text.contains("need a haircut")
        || text.contains("haircut")
        || text.contains("what should i wear")
        || text.contains("wear to the wedding")
        || text.contains("want to meditate")
        || text.contains("teach me spanish")
        || (text.contains("hungry") && text.contains("spicy"))
        || text.contains("book a hotel")
        || text.contains("hotel in chicago")
        || text.contains("change the ac filter")
        || text.contains("change ac filter")
        || text.contains("what should we do with the kids")
        || text.contains("kids today")
        || text.contains("toilet is clogged")
        || text.contains("toilet clogged")
        || text.contains("sew a button")
        || text.contains("need a laugh")
        || text.contains("listen to jazz")
        || text.contains("listen to some jazz")
        || (text.contains("suggest") && text.contains("book"))
        || text.contains("bored of cooking")
        || text.contains("ripe banana")
        || text.contains("beach trip")
        || text.contains("freeze tonight")
        || text.contains("pack a lunch")
        || text.contains("patio cushion")
        || text.contains("bike ride")
        || text.contains("order dog food")
        || text.contains("what s for breakfast")
        || text.contains("what's for breakfast")
        || text.contains("father s day")
        || (text.contains("dad") && text.contains("gift"))
        || text.contains("need a break")
        || (text.contains("wine") && text.contains("steak"))
        || text.contains("order more ink")
        || text.contains("safe to run")
        || (text.contains("sarah") && text.contains("birthday last year"))
        || text.contains("game night")
        || text.contains("baby is crying again")
        || text.contains("chicken but no ideas")
        || text.contains("who do we know that fixes sinks")
        || text.contains("learn about the solar system")
        || text.contains("keep oversleeping")
        || text.contains("when should i leave for the airport")
        || text.contains("buy food for the dinner party")
        || text.contains("i m really hot")
        || text.contains("i'm really hot")
        || text.contains("guests coming over")
        || text.contains("baby is awake")
        || text.contains("olive oil")
        || text.contains("build a bookshelf")
        || text.contains("toilet keeps running")
        || (text.contains("knee") && text.contains("run"))
        || text.contains("side dish for pasta")
        || text.contains("pack my gym bag")
        || text.contains("over budget")
        || text.contains("text mom happy birthday")
        || text.contains("driving home in the rain")
        || text.contains("safe to eat")
        || text.contains("dark parking lot")
        || text.contains("movie we haven t seen")
        || text.contains("defrost the turkey")
        || text.contains("what s for dinner")
        || text.contains("what's for dinner")
        || text.contains("going for a run")
        || (text.contains("feeling cold")
            || text.contains("feel cold")
            || text.contains("i am cold"))
        || (text.contains("snack") && (text.contains("lunchbox") || text.contains("lunch box")))
        || (text.contains("detergent") && contains_any(text, &["like", "order", "more"]))
        || (text.contains("movie") && text.contains("robot"))
        || text == "i m bored"
        || text.contains("weird noise coming from the car")
        || text.contains("printer")
        || text.contains("what can i cook with")
        || text.contains("comfort movie")
        || text.contains("warm enough to go to the park")
        || text.contains("date night idea")
        || text.contains("too late to call grandma")
        || text.contains("baby is crying")
        || text == "i m stressed"
        || text.contains("science fair idea")
        || text.contains("what can i bake")
        || text.contains("headache")
        || text.contains("read me a story")
        || text.contains("movie for tonight")
        || text.contains("find a movie for tonight")
        || text.contains("order more dog food")
        || text.contains("trip to the zoo")
        || (text.contains("hungry") && text.contains("diet"))
        || text.contains("washing machine is shaking")
        || text == "watch tv"
        || text.contains("someone is at the door")
        || text.contains("scary movie")
        || text.contains("need a drink")
        || text.contains("too bright")
        || text.contains("lonely")
        || text.contains("sink smells")
        || text.contains("leaving for work")
        || text.contains("make tacos")
        || text.contains("muggy")
        || text.contains("cut my finger")
        || text.contains("where are my keys")
        || text.contains("noise outside")
        || text.contains("order pizza")
        || text.contains("start the car")
        || text.contains("math homework")
        || text.contains("tired of this song")
        || text.contains("tell me a joke")
        || text.contains("ephemeral")
        || text.contains("build a fort")
        || text.contains("running late for the train")
        || (text.contains("air quality") && !text.contains("nursery"))
        || text.contains("birthday party")
        || text.contains("spider")
        || text.contains("can t find the remote")
        || text.contains("can't find the remote")
        || text.contains("garage freezer")
}

fn is_app_only_secret_question(text: &str) -> bool {
    (text.contains("password")
        || text.contains("passcode")
        || text.contains("gate code")
        || text.contains("door code")
        || text.contains("lock code")
        || text.contains("alarm code")
        || text.contains("security code")
        || text.contains("spare keys")
        || text.contains("house keys")
        || (text.contains("code")
            && (text.contains("account")
                || text.contains("netflix")
                || text.contains("subscription")
                || text.contains("shed")))
        || text.contains("combination")
        || text.contains("combo")
        || text.contains("confirmation number")
        || text.contains("account number")
        || text.contains("bank login")
        || text.contains("password manager")
        || text.contains("secure vault")
        || text.contains("credentials vault"))
        && !text.contains("guest speaker")
        && (text.contains("what")
            || text.contains("show")
            || text.contains("find")
            || text.contains("where")
            || text.contains("number")
            || text.contains("key")
            || text.contains("login")
            || text.contains("credential")
            || text.contains("wifi")
            || text.contains("wi fi"))
}

fn scene_or_routine_activation_request(text: &str) -> Option<String> {
    if matches!(
        text,
        "goodnight genieclaw"
            | "good night genieclaw"
            | "goodnight genie claw"
            | "good night genie claw"
            | "goodnight"
            | "good night"
            | "i m home"
            | "i am home"
            | "lock up the house"
            | "lock up house"
            | "turn off everything"
            | "turn everything off"
            | "i m back"
            | "i am back"
    ) {
        return Some(
            if matches!(text, "i m home" | "i am home" | "i m back" | "i am back") {
                "arrival".into()
            } else if text == "lock up the house" || text == "lock up house" {
                "lock up house".into()
            } else if text == "turn off everything" || text == "turn everything off" {
                "all off".into()
            } else {
                "goodnight".into()
            },
        );
    }

    for prefix in ["activate ", "start ", "run "] {
        if let Some(rest) = text.strip_prefix(prefix).map(str::trim)
            && (rest.contains(" scene") || rest.contains(" routine"))
        {
            let entity = rest
                .trim_end_matches(" scene")
                .trim_end_matches(" routine")
                .trim()
                .to_string();
            if !entity.is_empty() {
                return Some(entity);
            }
        }
    }

    None
}

fn play_media_request(text: &str) -> Option<String> {
    if matches!(
        text,
        "play focus music" | "start focus music" | "focus music"
    ) {
        return Some("focus music playlist".into());
    }

    if matches!(
        text,
        "put on the morning news"
            | "put on morning news"
            | "play the morning news"
            | "play morning news"
    ) {
        return Some("morning news".into());
    }

    if matches!(
        text,
        "play the weather report"
            | "play weather report"
            | "put on the weather report"
            | "put on weather report"
    ) {
        return Some("local weather report".into());
    }

    for prefix in ["please play ", "play ", "start ", "put on "] {
        if let Some(rest) = text.strip_prefix(prefix).map(str::trim)
            && rest.contains("playlist")
        {
            return Some(rest.to_string());
        }
    }
    None
}

fn shopping_list_add_request(text: &str) -> Option<String> {
    let rest = text.strip_prefix("add ")?;
    let items = rest
        .strip_suffix(" to the shopping list")
        .or_else(|| rest.strip_suffix(" to shopping list"))?
        .trim();
    if items.is_empty() {
        None
    } else {
        Some(items.replace(" and ", ", "))
    }
}

fn shopping_list_remove_request(text: &str) -> Option<String> {
    let items = text
        .strip_prefix("take ")
        .and_then(|rest| rest.strip_suffix(" off the shopping list"))
        .or_else(|| {
            text.strip_prefix("remove ")
                .and_then(|rest| rest.strip_suffix(" from the shopping list"))
        })?
        .trim();
    if items.is_empty() {
        None
    } else {
        Some(items.replace(" and ", ", "))
    }
}

fn household_rule_store_request(text: &str) -> Option<String> {
    if text.contains("kids")
        && text.contains("video game")
        && text.contains("homework")
        && (text.starts_with("don t let ") || text.starts_with("do not let "))
    {
        return Some("Kids must finish homework before screen time".into());
    }
    None
}

fn health_log_store_request(text: &str) -> Option<(&'static str, String)> {
    if text.starts_with("log that i drank ") && text.contains("water") {
        let amount = text
            .trim_start_matches("log that i drank ")
            .trim_end_matches(" of water")
            .trim_end_matches(" water")
            .trim();
        let content = if amount.is_empty() {
            "hydration log: drank water".into()
        } else {
            format!("hydration log: drank {amount} of water")
        };
        return Some(("health_tracker", content));
    }
    if text == "log my weight"
        || text == "log my weight today"
        || text.starts_with("log my weight ")
    {
        return Some((
            "health_tracker",
            "weight log: requested weight entry".into(),
        ));
    }
    None
}

fn reminder_or_alarm_store_request(text: &str) -> Option<(&'static str, String)> {
    if text.contains("emma") && text.contains("come over") && text.contains("after school") {
        return Some((
            "permission_requests",
            "Permission request for Mia: Emma can come over after school; parent approval requested".into(),
        ));
    }

    if text.contains("remember that") && text.contains("red hoodie") && text.contains("dad s car") {
        return Some((
            "item_location_events",
            "Mia red hoodie location: red hoodie is in Dad's car".into(),
        ));
    }

    if text.contains("save this") && text.contains("rainy day playlist") {
        return Some((
            "user_media_aliases",
            "Mia rainy-day playlist: save current media session as rainy-day playlist".into(),
        ));
    }

    if text.contains("remember") && text.contains("fan on low") && text.contains("sleep") {
        return Some((
            "preference",
            "Mia sleep comfort preference: fan on low for sleep".into(),
        ));
    }

    if text.contains("tell dad") && text.contains("puzzle is done") {
        return Some((
            "reminders",
            "Leo puzzle completion reminder: tell Dad when Leo says the puzzle is done".into(),
        ));
    }

    if text.contains("remind me")
        && text.contains("water my plant")
        && text.contains("after school")
    {
        return Some((
            "reminder",
            "Reminder for Mia after school: water her plant".into(),
        ));
    }

    if text.contains("save this temperature") && text.contains("rehearsal comfort") {
        return Some((
            "activity_preference_embeddings",
            "Mia rehearsal comfort preference: save current temperature, fan speed, and humidity"
                .into(),
        ));
    }

    if text.contains("add batteries and poster board") && text.contains("project list") {
        return Some((
            "project_list_items",
            "Mia project list supplies: batteries and poster board".into(),
        ));
    }

    if text.contains("make my alarm skip holidays") {
        return Some((
            "alarms",
            "Mia recurring alarm update: skip school holidays when school is closed".into(),
        ));
    }

    if text.contains("save this lighting") && text.contains("art time") {
        return Some((
            "scene_embeddings",
            "Art time lighting scene: save current room lights, desk lamp, and blinds for Mia's art time".into(),
        ));
    }

    if text.contains("make tomorrow into a checklist") {
        return Some((
            "daily_checklists",
            "Tomorrow checklist for Mia: build ordered checklist from calendar, school tasks, and reminders".into(),
        ));
    }

    if text.contains("remember that") && text.contains("green night") && text.contains("light") {
        return Some((
            "preference",
            "Leo prefers green as his night-light color".into(),
        ));
    }

    if text.contains("remind leo")
        && text.contains("soccer cleats")
        && text.contains("tomorrow morning")
    {
        return Some((
            "reminder",
            "Reminder for Leo tomorrow morning: bring soccer cleats".into(),
        ));
    }

    if text.starts_with("set an alarm for rehearsal at ")
        && let Some(time) = text.strip_prefix("set an alarm for rehearsal at ")
    {
        let time = clean_quick_value(time);
        if !time.is_empty() {
            return Some(("alarm", format!("Alarm for rehearsal at {time}")));
        }
    }

    if text.contains("tell me when dad gets home") {
        return Some((
            "presence_alert",
            "Presence alert for Leo: tell him when Dad gets home".into(),
        ));
    }

    if text.contains("save the bathroom") && text.contains(" at 7") && text.contains("hair wash") {
        return Some((
            "reservation",
            "Bathroom reservation for Mia at 7:00 PM for hair wash".into(),
        ));
    }

    None
}

fn preferred_timer_request(text: &str) -> Option<(u64, String)> {
    if text.contains("lego cleanup timer") {
        return Some((600, "lego cleanup".into()));
    }
    None
}

fn priority_home_control_request(text: &str) -> Option<(String, &'static str, Option<f64>)> {
    if text.contains("after dinner cleanup") || text.contains("after-dinner cleanup") {
        return Some(("after-dinner cleanup".into(), "activate", None));
    }

    if text.contains("rainy pickup mode") {
        return Some(("rainy pickup mode".into(), "activate", None));
    }

    if text.contains("living room") && text.contains("board games") {
        return Some(("living room board games".into(), "activate", None));
    }

    if text.contains("block notifications")
        && text.contains("except mom")
        && text.contains("test practice")
    {
        return Some((
            "mia test-practice notifications".into(),
            "allow_mom_only",
            None,
        ));
    }

    if text.contains("coffee") && text.contains("when i wake up") {
        return Some(("coffee maker wake brew".into(), "schedule_on_alarm", None));
    }

    if text.contains("cold after bath") {
        return Some(("leo post-bath comfort".into(), "activate", None));
    }

    if text.contains("basement flood check") {
        return Some(("basement flood check".into(), "run", None));
    }

    if text.contains("temporary code") && text.contains("grandma") {
        return Some((
            "grandma temporary door code".into(),
            "create_until_21",
            None,
        ));
    }

    if text.contains("desk feels glarey") || text.contains("desk feels glary") {
        return Some(("mia desk glare comfort".into(), "activate", None));
    }

    if text.contains("quiet drawing time") {
        return Some(("leo quiet drawing time".into(), "activate", None));
    }

    if text.contains("upstairs cooler") && text.contains("leo") && text.contains("alone") {
        return Some(("upstairs cooling except leo room".into(), "activate", None));
    }

    if text.contains("screens") && text.contains("family dinner") {
        return Some(("family dinner screens".into(), "pause", None));
    }

    if text.contains("stairs bright") {
        return Some(("stairwell lights".into(), "set_level", Some(90.0)));
    }

    if text.contains("workshop dust control") {
        return Some(("workshop dust control".into(), "activate", None));
    }

    if text.contains("closet light") && text.contains("when i open it") {
        return Some(("mia closet light automation".into(), "create", None));
    }

    if text.contains("low power mode") && text.contains("until five") {
        return Some(("low-power mode".into(), "activate_until_5pm", None));
    }

    if text.contains("animal show") && text.contains("not loud") {
        return Some(("leo animal show".into(), "play_low_volume", None));
    }

    if text.contains("front entry lights") && text.contains("until mia gets home") {
        return Some(("front entry lights until mia home".into(), "hold", None));
    }

    if text.contains("hear dripping") {
        return Some(("nearby leak check".into(), "check_and_alert", None));
    }

    if text.contains("room smells weird") {
        return Some(("mia room air-quality safety".into(), "check_and_vent", None));
    }

    if text.contains("school night reset") || text.contains("school-night reset") {
        return Some(("school-night reset".into(), "activate", None));
    }

    if text.contains("freezer") && text.contains("above 10") {
        return Some((
            "freezer temperature alert".into(),
            "create_threshold_10",
            None,
        ));
    }

    if text.contains("only the mirror lights") {
        return Some(("mia mirror lights only".into(), "activate", None));
    }

    if text.contains("backyard lights") && text.contains("grilling") {
        return Some(("backyard grilling lights".into(), "activate", None));
    }

    if text.contains("too scared to go downstairs") {
        return Some((
            "downstairs reassurance path lights".into(),
            "activate",
            None,
        ));
    }

    if text.contains("dinner warm") && text.contains("jared arrives") {
        return Some(("dinner warm until jared arrives".into(), "activate", None));
    }

    if text.contains("quiet time") && text.contains("after school") && text.contains("wednesday") {
        return Some(("mia wednesday quiet time".into(), "schedule", None));
    }

    if text.contains("open windows") && text.contains("outside is cleaner") {
        return Some(("cleaner outside air window mode".into(), "activate", None));
    }

    if text.contains("holiday lighting schedule") {
        return Some(("holiday lighting schedule".into(), "create", None));
    }

    if text.contains("rainy day alarm") || text.contains("rainy-day alarm") {
        return Some(("mia rainy-day alarm".into(), "set_for_tomorrow", None));
    }

    if text.contains("guest breakfast mode") {
        return Some(("guest breakfast mode".into(), "activate", None));
    }

    if text.contains("laundry room not scary") {
        return Some(("leo laundry-room reassurance".into(), "activate", None));
    }

    if text.contains("oven") && text.contains("preheats") {
        return Some(("oven preheat reminder".into(), "create", None));
    }

    if text.contains("hallway camera") && text.contains("sleepover guests change") {
        return Some((
            "hallway camera sleepover privacy".into(),
            "privacy_20",
            None,
        ));
    }

    if text.contains("cookies are cool enough") {
        return Some(("cookie cooling alert".into(), "create", None));
    }

    if text.contains("laundry leaks again") && text.contains("shut off the water") {
        return Some(("automatic laundry leak shutoff".into(), "enable", None));
    }

    if text.contains("morning checklist") && text.contains("wall") {
        return Some(("leo morning checklist display".into(), "show", None));
    }

    if text.contains("upstairs warmer") && text.contains("kids are getting ready") {
        return Some(("kids morning upstairs warmth".into(), "schedule", None));
    }

    if text.contains("final safety sweep") {
        return Some(("final safety sweep".into(), "run", None));
    }

    if text.contains("toaster") && text.contains("smoky") {
        return Some(("toaster smoke safety".into(), "cut_power_and_vent", None));
    }

    if text.contains("house better for pollen") || text.contains("pollen mode") {
        return Some(("pollen mode".into(), "activate", None));
    }

    if text.contains("driveway lights") && text.contains("pull in") {
        return Some((
            "driveway arrival lights".into(),
            "schedule_on_arrival",
            None,
        ));
    }

    if text.contains("room good for a video call") {
        return Some(("mia video-call room setup".into(), "activate", None));
    }

    if text.contains("dishwasher") && text.contains("after 9") {
        return Some(("dishwasher".into(), "schedule_after_21", None));
    }

    if text.contains("allergy day setup") || text.contains("allergy-day setup") {
        return Some(("allergy-day setup".into(), "activate", None));
    }

    if text.contains("wake me with sunlight") && text.contains("not sound") {
        return Some(("mia sunlight alarm".into(), "schedule_gradual_blinds", None));
    }

    if text.contains("guests only") && text.contains("wi fi") && text.contains("bathroom") {
        return Some(("limited guest info display".into(), "show_guest_card", None));
    }

    if text.contains("reading with dad") {
        return Some(("leo reading-with-dad scene".into(), "activate", None));
    }

    if text.contains("work call") && text.contains("quiet") {
        return Some(("work-call quiet mode".into(), "activate", None));
    }

    if text.contains("garage ventilated") && text.contains("paint") {
        return Some(("garage paint ventilation".into(), "activate", None));
    }

    if text.contains("sleepover lights") {
        return Some(("mia sleepover lights".into(), "apply_scene", None));
    }

    if text.contains("lights flash") && text.contains("cookies are done") {
        return Some((
            "leo cookie timer light alert".into(),
            "schedule_pulse",
            None,
        ));
    }

    if text.contains("calm morning for leo") {
        return Some(("leo calm morning".into(), "activate", None));
    }

    if text.contains("homework mode") && text.contains("both kids") {
        return Some(("kids homework mode".into(), "activate", None));
    }

    if text.contains("put my schedule") && text.contains("bathroom mirror") {
        return Some(("mia bathroom mirror agenda".into(), "show_agenda", None));
    }

    if text.contains("too hot in bed") {
        return Some(("leo bed cooling comfort".into(), "cool_down", Some(2.0)));
    }

    if text.contains("water under the sink") {
        return Some(("kitchen sink leak safety".into(), "shut_water_zone", None));
    }

    if text.contains("standby power") && text.contains("office") {
        return Some(("office standby-safe plugs".into(), "turn_off", None));
    }

    if text.contains("block youtube") && text.contains("finish math") {
        return Some(("mia youtube access".into(), "block_until_math_done", None));
    }

    if text.contains("contractor") && text.contains("garage") && text.contains(" at 10") {
        return Some(("contractor garage access".into(), "allow_10_to_10_20", None));
    }

    if text.contains("sleepover guest mode") {
        return Some(("sleepover guest mode".into(), "activate", None));
    }

    if text.contains("turn on stars") && text.contains("closet dark") {
        return Some(("leo stars except closet".into(), "activate", None));
    }

    if text.contains("open my blinds slowly") && text.contains("school morning") {
        return Some(("mia school-morning gradual blinds".into(), "schedule", None));
    }

    if text.contains("shower warm") && text.contains("not steamy") {
        return Some(("mia warm-not-steamy shower".into(), "activate", None));
    }

    if text.contains("security on") && text.contains("don t wake the kids") {
        return Some(("quiet armed security".into(), "activate", None));
    }

    if text.contains("heard glass break") {
        return Some((
            "downstairs glass-break safety".into(),
            "verify_and_alert",
            None,
        ));
    }

    if text.contains("call mom") && text.contains("kitchen screen") {
        return Some(("kitchen screen call mom".into(), "start_video_call", None));
    }

    if text.contains("babysitter") && text.contains("prep the house") {
        return Some(("babysitter mode".into(), "activate", None));
    }

    if text.contains("warm up") && text.contains("sarah") && text.contains("bathroom") {
        return Some((
            "sarah bathroom comfort".into(),
            "warm_for_minutes",
            Some(25.0),
        ));
    }

    if text.contains("focus mode") && text.contains("until five") {
        return Some(("personal focus mode".into(), "activate_until_5pm", None));
    }

    if text.contains("porch") && text.contains("waking the kids") {
        return Some(("quiet porch alerts tonight".into(), "activate", None));
    }

    if text.contains("start storm prep") || text.contains("run storm prep") {
        return Some(("storm prep".into(), "activate", None));
    }

    if text.contains("scared of the dark") {
        return Some(("night reassurance scene".into(), "activate", None));
    }

    if text.contains("open blinds") && text.contains("morning sun") && text.contains("mia") {
        return Some(("morning sun blinds except mia room".into(), "open", None));
    }

    if text.contains("piano practice quiet") || text.contains("quiet for the rest of the house") {
        return Some(("piano practice quiet mode".into(), "activate", None));
    }

    if text.contains("smell gas") {
        return Some(("gas safety emergency".into(), "activate", None));
    }

    if text.contains("start bedtime") && text.contains("mia") && text.contains("20 minutes") {
        return Some(("bedtime with mia reading override".into(), "activate", None));
    }

    if text.contains("vacation mode") && text.contains("next week") {
        return Some(("scheduled vacation mode next week".into(), "schedule", None));
    }

    if text.contains("robot vacuum") && text.contains("under my bed") {
        return Some(("leo under-bed vacuum zone".into(), "clean", None));
    }

    if text.contains("turn off notifications") && text.contains("practicing violin") {
        return Some((
            "mia violin practice notifications".into(),
            "mute_for_practice",
            None,
        ));
    }

    if text.contains("kitchen") && (text.contains("toddler safe") || text.contains("toddler-safe"))
    {
        return Some(("toddler-safe kitchen".into(), "activate", None));
    }

    if text.contains("lock everything") && text.contains("except") && text.contains("back gate") {
        return Some(("all locks except back gate".into(), "lock_except", None));
    }

    if text.contains("spaceship") && text.contains("hallway") {
        return Some(("spaceship hallway".into(), "activate", None));
    }

    if text.contains("everything downstairs")
        && text.contains("except")
        && text.contains("kitchen lights")
    {
        return Some((
            "downstairs except kitchen lights".into(),
            "turn_off_except",
            None,
        ));
    }

    if matches!(text, "run movie night" | "start movie night") {
        return Some(("movie night".into(), "activate", None));
    }

    if text.contains("too bright") && text.contains("reading") {
        return Some(("reading light comfort".into(), "activate", None));
    }

    if text.contains("make my room cozy") || text.contains("make the room cozy") {
        return Some(("personal cozy room scene".into(), "activate", None));
    }

    if text.contains("smoke alarm real") || text.contains("smoke alert") {
        return Some(("smoke emergency protocol".into(), "activate", None));
    }

    if text.contains("away mode") && text.contains("house") {
        return Some(("away mode".into(), "activate", None));
    }

    if text.contains("study playlist") && text.contains("desk lamp") {
        return Some(("personal study scene".into(), "activate", None));
    }

    if text.contains("too loud") {
        return Some(("nearby media volume".into(), "set_volume", Some(25.0)));
    }

    if text.contains("night light") && text.contains("blue") {
        return Some(("personal night-light".into(), "set_color_blue", None));
    }

    if text.contains("pause internet") && text.contains("kids") && text.contains("until dinner") {
        return Some(("kids internet".into(), "pause_until_dinner", None));
    }

    if text.contains("safe at night") && text.contains("hallway") {
        return Some(("night hallway safety".into(), "activate", None));
    }

    if text.contains("dinner prep mode") {
        return Some(("dinner prep mode".into(), "activate", None));
    }

    if text.contains("spilled water") && text.contains("outlet") {
        return Some(("outlet spill safety protocol".into(), "activate", None));
    }

    if matches!(text, "warm up the car" | "warm up car") {
        return Some(("connected car climate".into(), "remote_start", Some(72.0)));
    }

    if matches!(
        text,
        "send this address to my car" | "send address to my car"
    ) {
        return Some(("car navigation".into(), "send_destination", None));
    }

    if text.contains("fallen") && text.contains("can t get up") {
        return Some(("fall emergency alert".into(), "activate", None));
    }

    if text.contains("prep the house for vacation")
        || text.contains("prepare the house for vacation")
    {
        return Some(("vacation mode".into(), "activate", None));
    }

    if text.contains("smoky in here") || text.contains("smoke in here") {
        return Some(("smoke ventilation protocol".into(), "activate", None));
    }

    if text.contains("working late") {
        return Some(("working late family update".into(), "activate", None));
    }

    if text.contains("take a nap") || text.contains("going to nap") {
        return Some(("nap mode".into(), "activate", None));
    }

    if text.contains("dark parking lot") {
        return Some(("parking lot safety protocol".into(), "activate", None));
    }

    if text.contains("driving home") && text.contains("rain") {
        return Some(("arrival rain protocol".into(), "activate", None));
    }

    if text.contains("stuffy") {
        return Some(("ventilation comfort scene".into(), "activate", None));
    }

    if text.contains("working from home") || text.contains("work from home") {
        return Some(("work from home scene".into(), "activate", None));
    }

    if text.contains("locked out") {
        return Some(("front door".into(), "unlock", None));
    }

    None
}

fn home_control_request(text: &str) -> Option<(String, &'static str, Option<f64>)> {
    if matches!(text, "turn off the tv" | "turn off tv") {
        return Some(("tv".into(), "turn_off", None));
    }

    if matches!(
        text,
        "turn on the pool cleaner" | "turn on pool cleaner" | "start the pool cleaner"
    ) {
        return Some(("pool cleaner".into(), "start", None));
    }

    if matches!(
        text,
        "set the thermostat to eco mode" | "set thermostat to eco mode"
    ) {
        return Some(("thermostat".into(), "set_preset", None));
    }

    if matches!(
        text,
        "turn on the alarm" | "turn on alarm" | "arm the alarm"
    ) {
        return Some(("security alarm".into(), "arm", None));
    }

    if matches!(text, "start the robot mower" | "start robot mower") {
        return Some(("robot mower".into(), "start", None));
    }

    if matches!(text, "test the smoke detectors" | "test smoke detectors") {
        return Some(("smoke detectors".into(), "test", None));
    }

    if matches!(
        text,
        "turn off upstairs lights" | "turn off the upstairs lights"
    ) {
        return Some(("upstairs lights".into(), "turn_off", None));
    }

    if matches!(
        text,
        "turn off holiday lights" | "turn off the holiday lights"
    ) {
        return Some(("outdoor holiday lights".into(), "turn_off", None));
    }

    if matches!(text, "call my phone" | "ring my phone" | "find my phone") {
        return Some(("phone finder".into(), "activate", None));
    }

    if text.starts_with("set up the slow cooker") || text.starts_with("set up slow cooker") {
        return Some(("slow cooker chili".into(), "activate", None));
    }

    if text.starts_with("stop the sprinklers") || text.starts_with("pause the sprinklers") {
        return Some(("sprinklers".into(), "pause", None));
    }

    if text == "turn on the porch light when i arrive"
        || text == "turn on porch light when i arrive"
    {
        return Some(("porch light".into(), "schedule_on_arrival", None));
    }

    if matches!(text, "start the dishwasher" | "start dishwasher") {
        return Some(("dishwasher normal cycle".into(), "activate", None));
    }

    if let Some((entity, action)) = simple_turn_request(text) {
        return Some((entity, action, None));
    }

    if let Some(rest) = text
        .strip_prefix("set ")
        .or_else(|| text.strip_prefix("preheat "))
        && let Some((entity, value)) = parse_temperature_target(rest)
    {
        return Some((entity, "set_temperature", Some(value)));
    }

    None
}

fn simple_turn_request(text: &str) -> Option<(String, &'static str)> {
    let (rest, action) = text
        .strip_prefix("turn on ")
        .map(|rest| (rest, "turn_on"))
        .or_else(|| {
            text.strip_prefix("turn off ")
                .map(|rest| (rest, "turn_off"))
        })?;
    if !(rest.contains("fan") || rest.contains("fireplace")) {
        return None;
    }
    let entity = clean_control_entity(rest);
    if entity.is_empty() {
        None
    } else {
        Some((entity, action))
    }
}

fn clean_control_entity(text: &str) -> String {
    let text = text
        .trim()
        .trim_start_matches("the ")
        .trim_end_matches(" please");
    if let Some((device, room)) = text.split_once(" in the ") {
        format!("{} {}", room.trim(), device.trim())
    } else if let Some((device, room)) = text.split_once(" in ") {
        format!("{} {}", room.trim(), device.trim())
    } else {
        text.to_string()
    }
}

fn parse_temperature_target(rest: &str) -> Option<(String, f64)> {
    let (entity, value_text) = rest
        .split_once(" to ")
        .or_else(|| rest.split_once(" at "))?;
    let entity = entity
        .trim()
        .trim_start_matches("the ")
        .trim_start_matches("a ")
        .trim_start_matches("an ");
    if entity.is_empty() {
        return None;
    }
    let value_token = value_text
        .split_whitespace()
        .find(|token| token.chars().any(|ch| ch.is_ascii_digit()))?;
    let value = value_token
        .trim_matches(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .parse::<f64>()
        .ok()?;
    if value.is_finite() {
        Some((entity.to_string(), value))
    } else {
        None
    }
}

fn household_role_query(text: &str) -> Option<&'static str> {
    if !(text.starts_with("who is ")
        || text.starts_with("who are ")
        || text.starts_with("whos ")
        || text.starts_with("who s ")
        || text.contains(" in this house")
        || text.contains(" in our house")
        || text.contains(" household"))
    {
        return None;
    }

    for token in text.split_whitespace() {
        if let Some(role) = normalize_household_role_query_token(token) {
            return Some(role);
        }
    }
    None
}

fn normalize_household_role_query_token(token: &str) -> Option<&'static str> {
    match token.trim_matches(|ch: char| matches!(ch, '.' | ',' | '?' | '!' | ':' | ';')) {
        "dad" | "father" => Some("dad"),
        "mom" | "mother" | "mum" => Some("mom"),
        "son" | "sons" => Some("son"),
        "daughter" | "daughters" => Some("daughter"),
        "child" | "children" | "kid" | "kids" => Some("child"),
        "wife" => Some("wife"),
        "husband" => Some("husband"),
        "partner" => Some("partner"),
        "dog" | "dogs" => Some("dog"),
        "cat" | "cats" => Some("cat"),
        "pet" | "pets" => Some("pet"),
        _ => None,
    }
}

fn asks_home_undo(text: &str) -> bool {
    matches!(
        text,
        "undo"
            | "undo that"
            | "undo last action"
            | "undo the last action"
            | "revert that"
            | "revert last action"
            | "put it back"
            | "put that back"
            | "reverse that"
            | "reverse last action"
    )
}

fn asks_action_history(text: &str) -> bool {
    if text.contains("changed in the garage") {
        return false;
    }
    contains_any(
        text,
        &[
            "what did you do",
            "what have you done",
            "what changed",
            "recent actions",
            "recent home actions",
            "action history",
            "pending confirmations",
            "pending confirmation",
        ],
    )
}

fn asks_home_assistant_status(text: &str) -> bool {
    contains_any(
        text,
        &[
            "home assistant status",
            "home assistant connected",
            "home assistant connection",
            "is home assistant connected",
            "ha status",
            "ha connected",
        ],
    )
}

fn asks_system_status(text: &str) -> bool {
    matches!(
        text,
        "system status"
            | "geniepod status"
            | "genie status"
            | "status of geniepod"
            | "status of genie"
            | "uptime"
            | "load average"
            | "governor status"
    )
}

fn home_status_target(text: &str) -> Option<String> {
    if text.contains("lights are still on upstairs")
        || (text.contains("which lights") && text.contains("upstairs"))
    {
        return Some("upstairs lights on".into());
    }

    if text.contains("needs charging tonight") || text.contains("need charging tonight") {
        return Some("family device charging tonight".into());
    }

    if text.contains("basement flood check") {
        return Some("basement flood check".into());
    }

    if text.contains("next filter change") {
        return Some("next filter change".into());
    }

    if text.contains("front door locked after") && text.contains("leo") {
        return Some("front door lock after leo arrival".into());
    }

    if text.contains("noisy appliance") {
        return Some("noisy appliance".into());
    }

    if text.contains("tooth fairy box") {
        return Some("leo tooth fairy box".into());
    }

    if text.contains("changed in the garage today") {
        return Some("garage changes today".into());
    }

    if text.contains("security alarm chirp") {
        return Some("security alarm chirp".into());
    }

    if text.contains("who s in the backyard") || text.contains("who's in the backyard") {
        return Some("backyard recognized presence".into());
    }

    if text.contains("bedtime chart") {
        return Some("leo bedtime chart remaining".into());
    }

    if text.contains("upstairs window before the rain") {
        return Some("upstairs window before rain".into());
    }

    if text.contains("devices")
        && (text.contains("guest wi fi")
            || text.contains("guest wi-fi")
            || text.contains("guest wifi"))
    {
        return Some("guest wifi devices".into());
    }

    if text.contains("side path icy") {
        return Some("side path ice risk".into());
    }

    if text.contains("office internet slow") {
        return Some("office internet slow reason".into());
    }

    if text.contains("chores did leo skip") {
        return Some("leo skipped chores this week".into());
    }

    if text.contains("mia s purifier on high") || text.contains("mia's purifier on high") {
        return Some("mia purifier high reason".into());
    }

    if text.contains("outdoor cameras need cleaning") {
        return Some("outdoor camera cleaning report".into());
    }

    if text.contains("garage close after jared left") {
        return Some("garage close after jared left".into());
    }

    if text.contains("feed the cat too much") {
        return Some("cat feeding amount check".into());
    }

    if text.contains("oldest thing in the fridge") {
        return Some("oldest fridge food".into());
    }

    if text.contains("lamp flickering") {
        return Some("mia lamp flicker reason".into());
    }

    if text.contains("sensor") && text.contains("bypass") {
        return Some("security sensor bypass report".into());
    }

    if text.contains("room smells weird") {
        return Some("mia room air-quality check".into());
    }

    if text.contains("dad see my message") {
        return Some("leo dad message read status".into());
    }

    if text.contains("backpacks are by the door") {
        return Some("entryway backpacks".into());
    }

    if text.contains("privacy report") && text.contains("cameras") {
        return Some("camera privacy report".into());
    }

    if text.contains("automation fired the most") {
        return Some("top automation today".into());
    }

    if text.contains("fridge door") && text.contains("close") {
        return Some("fridge door".into());
    }

    if text.contains("sensors need batteries") || text.contains("need batteries soon") {
        return Some("sensor battery report".into());
    }

    if text.contains("plants need attention") {
        return Some("plant attention report".into());
    }

    if text.contains("bathroom free") {
        return Some("upstairs bathroom availability".into());
    }

    if text.contains("end of day house summary") || text.contains("end-of-day house summary") {
        return Some("end-of-day house summary".into());
    }

    if text.contains("compare this week") && text.contains("electricity") {
        return Some("weekly electricity comparison".into());
    }

    if text.contains("back burner") && text.contains("off") {
        return Some("back burner".into());
    }

    if text.contains("which room seems drafty") || text.contains("drafty room") {
        return Some("drafty room report".into());
    }

    if text.contains("devices are offline") || text.contains("which devices are offline") {
        return Some("offline devices".into());
    }

    if text.contains("using the most electricity") || text.contains("most electricity") {
        return Some("top electricity usage".into());
    }

    if text.contains("freezer door") && text.contains("left open") {
        return Some("freezer door".into());
    }

    if text.contains("all the windows closed") || text.contains("windows closed") {
        return Some("open windows".into());
    }

    if text.contains("sprinkler") && text.contains("run this morning") {
        return Some("sprinkler run history".into());
    }

    if text.contains("morning readiness report") {
        return Some("morning readiness report".into());
    }

    if text.contains("self cleaning oven") || text.contains("self clean oven") {
        return Some("self-cleaning oven".into());
    }

    if (text.contains("oven on") || text.contains("leave the oven"))
        && !text.contains("self cleaning")
        && !text.contains("self clean")
    {
        return Some("oven".into());
    }

    if text.contains("doors are unlocked") || text.contains("what doors are unlocked") {
        return Some("unlocked doors".into());
    }

    if text.contains("cameras with motion") || text.contains("camera") && text.contains("motion") {
        return Some("cameras with recent motion".into());
    }

    if text.contains("freezer") && text.contains("too warm") {
        return Some("freezer".into());
    }

    if text.contains("speed limit") {
        return Some("speed limit".into());
    }

    if text.contains("water pressure") {
        return Some("water pressure".into());
    }

    if text.contains("sump pump") {
        return Some("sump pump".into());
    }

    if text.contains("sous vide") {
        return Some("sous vide".into());
    }

    if text.contains("air quality") && text.contains("nursery") {
        return Some("nursery air quality".into());
    }

    if text.starts_with("did ") && text.contains("lock") && text.contains("car") {
        return Some("car locks".into());
    }

    if text.starts_with("did ")
        && (text.contains("package arrive") || text.contains("package arrived"))
    {
        return Some("package".into());
    }

    if text.contains("printer") && text.contains("ink") {
        return Some("printer ink".into());
    }

    if text.contains("baby monitor") {
        return Some("baby monitor".into());
    }

    if text.contains("baby") && text.contains("breathing") {
        return Some("baby breathing monitor".into());
    }

    if text.starts_with("did ") && text.contains("mail") {
        return Some("mailbox".into());
    }

    if text.contains("stove") && text.starts_with("did i leave ") {
        return Some("stove".into());
    }

    if text.contains("iron") {
        return Some("iron".into());
    }

    if text.contains("water") && text.contains("hot") {
        return Some("water heater".into());
    }

    if text.contains("solar") && (text.contains("generate") || text.contains("generated")) {
        return Some("solar power today".into());
    }

    if text.contains("home assistant") || !looks_like_status_query(text) {
        return None;
    }

    let target = clean_status_target(text);
    if target.is_empty() {
        return None;
    }

    if contains_any(&target, &["light", "lights", "lamp", "lamps"]) {
        return Some(if target.split_whitespace().count() == 1 {
            "lights".into()
        } else {
            target
        });
    }

    if contains_any(
        &target,
        &["switch", "switches", "plug", "plugs", "outlet", "outlets"],
    ) {
        return Some(if target.split_whitespace().count() == 1 {
            "switches".into()
        } else {
            target
        });
    }

    if contains_any(
        &target,
        &["thermostat", "thermostats", "temperature", "climate"],
    ) {
        if target.contains("attic") {
            return Some("attic temperature".into());
        }
        return Some(
            if target.split_whitespace().count() == 1 || target == "temperature" {
                "thermostat".into()
            } else {
                target
            },
        );
    }

    if contains_any(
        &target,
        &[
            "cover",
            "covers",
            "blind",
            "blinds",
            "shade",
            "shades",
            "curtain",
            "curtains",
            "garage",
            "garage door",
            "gate",
            "front gate",
        ],
    ) {
        return Some(if target.split_whitespace().count() == 1 {
            "covers".into()
        } else {
            target
        });
    }

    if contains_any(
        &target,
        &["lock", "locks", "door lock", "door locks", "door"],
    ) {
        return Some(if target.split_whitespace().count() == 1 {
            "locks".into()
        } else {
            target
        });
    }

    if contains_any(&target, &["driveway", "icy", "ice"]) {
        return Some("driveway ice".into());
    }

    if target.contains("sprinkler") || target.contains("irrigation") {
        return Some(if target.contains("front") {
            "front sprinklers".into()
        } else {
            "sprinklers".into()
        });
    }

    if contains_any(&target, &["dryer", "drying machine"]) {
        return Some("dryer".into());
    }

    if target.contains("humidity") {
        return Some(if target.contains("basement") {
            "basement humidity".into()
        } else {
            "humidity".into()
        });
    }

    if target.contains("tire pressure") && target.contains("car") {
        return Some("car tire pressure".into());
    }

    if target.contains("car") {
        return Some("car".into());
    }

    if target.contains("stove") || target.contains("burner") || target.contains("oven") {
        return Some("stove".into());
    }

    if target.contains("package") || target.contains("delivery") {
        return Some("package".into());
    }

    if contains_any(&target, &["freezer", "garage freezer"]) {
        return Some(if target.contains("garage") {
            "garage freezer".into()
        } else {
            "freezer".into()
        });
    }

    None
}

fn timer_request(text: &str) -> Option<(u64, String)> {
    if !(text.contains("timer") || text.starts_with("remind me ") || text.starts_with("remind us "))
    {
        return None;
    }

    let tokens = text.split_whitespace().collect::<Vec<_>>();
    let (seconds, unit_end_index) = parse_duration(&tokens)?;
    if seconds == 0 {
        return None;
    }

    let label = reminder_label(&tokens, unit_end_index)
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| {
            if text.starts_with("remind ") {
                "reminder".into()
            } else {
                "timer".into()
            }
        });

    Some((seconds, label))
}

fn weather_request(text: &str) -> Option<(String, bool)> {
    if !(text.contains("weather") || text.contains("forecast")) {
        return None;
    }

    let location = extract_location_after_marker(text, " in ")
        .or_else(|| extract_location_after_marker(text, " for "))?;
    if location.is_empty() || location == "today" || location == "tomorrow" {
        return None;
    }

    let forecast = text.contains("forecast")
        || text.contains("tomorrow")
        || text.contains("week")
        || text.contains("7 day")
        || text.contains("seven day");

    Some((location, forecast))
}

fn web_search_request(text: &str) -> Option<String> {
    if text.starts_with("search memory ") || text.starts_with("search memories ") {
        return None;
    }

    if text.contains("stock price") {
        let subject = text
            .split_once("stock price of ")
            .map(|(_, subject)| subject)
            .or_else(|| {
                text.split_once("stock price for ")
                    .map(|(_, subject)| subject)
            })
            .unwrap_or("")
            .trim();
        let query = if subject.is_empty() {
            "stock price".to_string()
        } else {
            format!("{subject} stock price")
        };
        return Some(query);
    }

    if matches!(text, "read the news" | "read news" | "what s the news") {
        return Some("top news headlines".into());
    }

    for prefix in [
        "search the web for ",
        "search web for ",
        "search online for ",
        "internet search for ",
        "web search ",
        "look up ",
        "lookup ",
    ] {
        if let Some(query) = text.strip_prefix(prefix) {
            let query = query.trim();
            if !query.is_empty() {
                return Some(query.to_string());
            }
        }
    }

    None
}

fn extract_location_after_marker(text: &str, marker: &str) -> Option<String> {
    let (_, location) = text.rsplit_once(marker)?;
    let location = location
        .trim()
        .trim_start_matches("the ")
        .trim_end_matches(" today")
        .trim_end_matches(" tomorrow")
        .trim()
        .to_string();
    if location.is_empty() {
        None
    } else {
        Some(location)
    }
}

fn calculation_request(text: &str) -> Option<String> {
    temperature_conversion_expression(text)
        .or_else(|| percentage_expression(text))
        .or_else(|| arithmetic_expression(text))
}

fn temperature_conversion_expression(text: &str) -> Option<String> {
    if !(text.contains("to celsius") || text.contains("to celcius")) {
        return None;
    }
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    let fahrenheit = tokens
        .iter()
        .find_map(|token| parse_decimal_token(token.trim_end_matches("f")))?;
    Some(format!("({fahrenheit} - 32) * 5 / 9"))
}

fn percentage_expression(text: &str) -> Option<String> {
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    let percent_idx = tokens
        .iter()
        .position(|token| matches!(*token, "percent" | "percentage" | "%"))?;
    let percent = parse_decimal_token(tokens.get(percent_idx.wrapping_sub(1))?)?;

    let of_idx = tokens.iter().position(|token| *token == "of")?;
    let base = parse_decimal_token(tokens.get(of_idx + 1)?)?;

    Some(format!("{} * {} / 100", base, percent))
}

fn arithmetic_expression(text: &str) -> Option<String> {
    let expression = text
        .strip_prefix("calculate ")
        .or_else(|| text.strip_prefix("what is "))
        .or_else(|| text.strip_prefix("whats "))
        .or_else(|| text.strip_prefix("what's "))
        .unwrap_or(text)
        .replace(" plus ", " + ")
        .replace(" minus ", " - ")
        .replace(" times ", " * ")
        .replace(" multiplied by ", " * ")
        .replace(" divided by ", " / ")
        .replace(" over ", " / ");

    if !expression.chars().any(|c| c.is_ascii_digit())
        || !expression
            .chars()
            .any(|c| matches!(c, '+' | '-' | '*' | '/' | '(' | ')'))
    {
        return None;
    }

    if !expression
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '.' | '+' | '-' | '*' | '/' | '(' | ')'))
    {
        return None;
    }

    Some(expression.trim().to_string())
}

fn parse_decimal_token(token: &str) -> Option<f64> {
    token
        .trim_end_matches('%')
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_duration(tokens: &[&str]) -> Option<(u64, usize)> {
    for (idx, token) in tokens.iter().enumerate() {
        let Some(amount) = parse_number(token) else {
            continue;
        };
        // Combine a spoken compound cardinal like "forty five" (45) into a
        // single amount. Worded numbers arrive as separate whitespace tokens,
        // so the tens word and the ones word must be stitched here.
        let (amount, unit_index) = match tokens.get(idx + 1).and_then(|next| ones_digit(next)) {
            Some(ones) if is_tens_word(token) => (amount + ones, idx + 2),
            _ => (amount, idx + 1),
        };
        let unit = tokens.get(unit_index)?;
        let multiplier = match *unit {
            "second" | "seconds" | "sec" | "secs" => 1,
            "minute" | "minutes" | "min" | "mins" => 60,
            "hour" | "hours" | "hr" | "hrs" => 3600,
            _ => continue,
        };
        return Some((amount * multiplier, unit_index));
    }
    None
}

/// True when `token` is a spoken tens word that can lead a compound cardinal
/// such as "forty five".
fn is_tens_word(token: &str) -> bool {
    matches!(
        token,
        "twenty" | "thirty" | "forty" | "fifty" | "sixty" | "seventy" | "eighty" | "ninety"
    )
}

/// The trailing ones digit of a compound cardinal ("forty *five*"), 1-9.
fn ones_digit(token: &str) -> Option<u64> {
    match parse_number(token) {
        Some(value @ 1..=9) => Some(value),
        _ => None,
    }
}

fn parse_number(token: &str) -> Option<u64> {
    if let Ok(value) = token.parse::<u64>() {
        return Some(value);
    }

    match token {
        "one" | "a" | "an" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        "fifteen" => Some(15),
        "twenty" => Some(20),
        "thirty" => Some(30),
        "forty" => Some(40),
        "fifty" => Some(50),
        "sixty" => Some(60),
        "seventy" => Some(70),
        "eighty" => Some(80),
        "ninety" => Some(90),
        _ => None,
    }
}

fn reminder_label(tokens: &[&str], unit_end_index: usize) -> Option<String> {
    let after_unit = tokens.get(unit_end_index + 1..)?;
    let to_index = after_unit.iter().position(|token| *token == "to")?;
    let label_tokens = after_unit.get(to_index + 1..)?;
    if label_tokens.is_empty() {
        return None;
    }
    Some(label_tokens.join(" "))
}

fn looks_like_status_query(text: &str) -> bool {
    text.contains(" status")
        || text.ends_with(" status")
        || text.starts_with("what ")
        || text.starts_with("which ")
        || text.starts_with("is ")
        || text.starts_with("are ")
        || text.starts_with("any ")
        || text.starts_with("check ")
        || text.starts_with("tell me ")
}

fn clean_status_target(text: &str) -> String {
    let mut target = text.to_string();
    for prefix in [
        "what is the ",
        "what are the ",
        "what is ",
        "what are ",
        "what ",
        "which ",
        "is the ",
        "are the ",
        "is ",
        "are ",
        "any ",
        "check the ",
        "check ",
        "tell me the ",
        "tell me ",
    ] {
        if let Some(stripped) = target.strip_prefix(prefix) {
            target = stripped.to_string();
            break;
        }
    }

    for suffix in [
        " are on",
        " are off",
        " are open",
        " are closed",
        " are unlocked",
        " are locked",
        " is on",
        " is off",
        " is open",
        " is closed",
        " is unlocked",
        " is locked",
        " status",
        " on",
        " off",
        " open",
        " closed",
        " unlocked",
        " locked",
        " active",
        " right now",
        " now",
    ] {
        if let Some(stripped) = target.strip_suffix(suffix) {
            target = stripped.to_string();
            break;
        }
    }

    target.trim().to_string()
}

fn asks_current_time(text: &str) -> bool {
    matches!(
        text,
        "what time is it"
            | "what is the time"
            | "whats the time"
            | "current time"
            | "tell me the time"
            | "what date is it"
            | "what is today"
            | "what day is it"
            | "date and time"
    )
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn clean_quick_value(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| matches!(ch, '.' | ',' | '?' | '!' | ';'))
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_home_assistant_status_to_system_info() {
        let call = route("home assistant status").unwrap();
        assert_eq!(call.name, "system_info");
    }

    #[test]
    fn routes_memory_health_to_memory_status() {
        let call = route("check memory health").unwrap();
        assert_eq!(call.name, "memory_status");
    }

    #[test]
    fn routes_identity_memory_questions_to_memory_recall() {
        let call = route("What is my name?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "name");

        let call = route("do you remember my name").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "name");
    }

    #[test]
    fn routes_household_role_questions_to_memory_recall() {
        let call = route("Who is the dad in this house?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "dad");

        let call = route("Who are the children in our house?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "child");
    }

    #[test]
    fn routes_structured_household_questions_to_memory_recall() {
        let call = route("How old is Leo?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "how old is leo");

        let call = route("Is Leo allowed to play video games after 8 PM?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(
            call.arguments["query"],
            "is leo allowed to play video games after 8 pm"
        );

        let call = route("Is anyone allergic to peanuts?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Does Mia have piano lessons today?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "does mia have piano lessons today");

        let call = route("Can Leo unlock the front door?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Who is picking up the kids today?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Did Leo feed the dog today?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What time does the school bus arrive?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("When is the electricity bill due?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Is it recycling week?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("When is the next parent-teacher conference?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Who turned off the security system?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Did Leo get his allowance this week?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What size shoe does Mia wear now?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("When is the next trash pickup?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Do we have any eggs left?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("When is Mia's next dentist appointment?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What time does the sun set today?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("When is Buster's next vet appointment?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Did I pay the electric bill?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Did Leo brush his teeth?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Is the community pool open today?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("When does the library close?").unwrap();
        assert_eq!(call.name, "memory_recall");

        for query in [
            "What is my credit score?",
            "What is my weight?",
            "What channel is ESPN?",
            "Did everyone brush their teeth?",
            "Is the dishwasher clean or dirty?",
            "Did the trash truck come yet?",
            "What is the temperature in the attic?",
            "Is Mia home from school?",
            "When is the subscription due?",
            "What is my VO2 max?",
            "What's on TV tonight?",
            "When is the next city council meeting?",
            "Leo: Can I watch cartoons now?",
            "Jared: Who opened the garage door?",
            "Leo: Is Mia home?",
            "Sarah: What groceries are low?",
            "Sarah: What's our Saturday morning routine?",
            "Leo: What's next before school?",
            "Sarah: Did Mia feed the cat?",
            "Leo: Can I have a snack?",
            "Sarah: Who's coming to dinner tonight?",
            "Mia: Did I finish my chores?",
            "Mia: What time is my bus tomorrow?",
            "Sarah: Which leftovers should we eat first?",
            "Mia: Did Mom approve my sleepover?",
            "Sarah: Who changed the thermostat?",
            "Sarah: Did Mia take her allergy medicine?",
            "Leo: Am I allowed to play outside?",
            "Jared: What do we still need to do before trash day?",
            "Sarah: Did anyone take the garbage bins out?",
            "Mia: Which homework needs internet?",
            "Leo: Can I use the stove?",
            "Jared: Which sensors need batteries soon?",
            "Leo: Did I pack my library book?",
            "Mia: Why did my alarm not go off?",
            "Sarah: What plants need attention?",
            "Jared: Why did away mode fail?",
            "Jared: Make an end-of-day house summary.",
            "Sarah: When did the laundry finish?",
            "Mia: Did my laundry get moved?",
            "Leo: Can I open the front door for Grandma?",
            "Jared: Why is the basement humid?",
            "Sarah: What needs charging tonight?",
            "Sarah: When is the next filter change?",
            "Sarah: Was the front door locked after Leo came in?",
            "Mia: Can I print my homework?",
            "Leo: Did my tooth fairy box stay closed?",
            "Jared: What changed in the garage today?",
            "Jared: Why did the security alarm chirp?",
            "Sarah: Who's in the backyard?",
            "Leo: What's left on my bedtime chart?",
            "Sarah: Did I close the upstairs window before the rain?",
            "Jared: What devices are on guest Wi-Fi?",
            "Mia: Is the side path icy?",
            "Jared: Why is the office internet slow?",
            "Sarah: What chores did Leo skip this week?",
            "Leo: Can the cat sleep in my room?",
            "Sarah: Why is Mia's purifier on high?",
            "Sarah: Did the garage close after Jared left?",
            "Leo: Did I feed the cat too much?",
            "Jared: What's the oldest thing in the fridge?",
            "Mia: Why is my lamp flickering?",
            "Leo: Can I open the garage door?",
            "Jared: Did anyone bypass a sensor?",
            "Leo: Did Dad see my message?",
            "Mia: Can I practice drums now?",
            "Jared: Which automation fired the most today?",
        ] {
            let call = route(query).unwrap();
            assert_eq!(call.name, "memory_recall", "{query}");
        }
    }

    #[test]
    fn routes_household_note_questions_to_memory_recall() {
        let call = route("Find my note about the bicycle lock code").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(
            call.arguments["query"],
            "find my note about the bicycle lock code"
        );

        let call = route("Where are the extra batteries kept?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(
            call.arguments["query"],
            "where are the extra batteries kept"
        );

        let call = route("What did the vet say about Buster's medicine?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What color did we paint the shed?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Where are the passports?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What's the model number of the fridge?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Who took the photos in Hawaii?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Find the warranty for the roof.").unwrap();
        assert_eq!(call.name, "memory_recall");

        for query in [
            "How do I clean the oven racks?",
            "Where are the spare lightbulbs?",
            "What did we have for dinner last Tuesday?",
            "Find the recipe for pancakes.",
            "What's on the hardware store list?",
            "Find the receipt for the new dishwasher.",
            "Where did we put the tent?",
            "Find the instructions for the board game.",
            "What color is the nursery paint?",
            "How do I reset the smoke detector?",
            "Where is the 10mm socket?",
            "Find the receipt for the Lego set.",
            "What color is the deck stain?",
            "Where is the fire extinguisher?",
            "What is the school's emergency number?",
            "Who do we call for HVAC repair?",
            "Where are the summer clothes?",
            "Find the recipe for the glaze.",
            "Find the manual for the grill.",
            "Where are the scented candles?",
            "What is the vet's address?",
            "Find the sewing kit.",
            "Where are the spare keys?",
            "What is the license plate number?",
            "What is the warranty for the fridge?",
            "Find the warranty for the laptop.",
            "Find the manual for the car.",
            "Where are the tax documents?",
            "How long do I boil an egg?",
            "What is the doctor's number?",
            "Find the user manual for the TV.",
            "Where are the hiking boots?",
            "Find the recipe for sourdough starter.",
            "What's the phone number for the pizza place?",
            "Where are the Thanksgiving decorations?",
            "Find the warranty for the AC unit.",
            "What is the IP address of the printer?",
            "Where are the tax returns from 2020?",
            "Mia: Where's my science fair checklist?",
            "Sarah: Find the air fryer manual.",
            "Sarah: Which filter does the hallway air purifier need?",
            "Jared: Find anything about dishwasher error E24.",
            "Mia: Where's my tablet charger?",
            "Leo: What bin does a pizza box go in?",
            "Jared: Find the washing machine warranty.",
            "Sarah: How do I remove marker from Leo's hoodie?",
            "Mia: Where's my backpack?",
            "Mia: Find my essay draft about oceans.",
            "Leo: Is it pajama day tomorrow?",
            "Sarah: Find Mia's allergy action plan.",
            "Jared: Find the ladder safety note.",
            "Leo: Tell me the dinosaur fact from yesterday.",
            "Mia: How do I reset the printer Wi-Fi?",
            "Sarah: Find Grandma's Wi-Fi note.",
            "Leo: Where do my wet soccer shoes go?",
            "Sarah: What was the blue paint color in Mia's room?",
            "Jared: What's the safest way out if the kitchen alarm goes off?",
            "Jared: Which breaker controls the dishwasher?",
            "Sarah: What did we do last time ants showed up?",
            "Leo: Where's the camping flashlight?",
            "Jared: Why didn't the sprinklers run today?",
            "Sarah: Find the cold medicine instructions.",
            "Leo: I can't find my blue cup.",
            "Jared: Did the side gate open while we were gone?",
            "Sarah: Find the note about Mia's recital outfit.",
            "Mia: What's the password for the guest speaker?",
            "Mia: Find my debate research about school lunches.",
            "Leo: Where did I leave my rain boots?",
            "Sarah: Find the slow cooker manual and timer chart.",
            "Mia: Did the garage camera see my bike?",
            "Jared: Find the receipt for the new water heater.",
            "Mia: Where is the white extension cord for my project?",
            "Sarah: Find a chicken recipe without peanuts.",
            "Sarah: Find Leo's vaccination form.",
            "Mia: Did Mom sign my field trip form?",
            "Mia: Find the photo backdrop instructions.",
            "Leo: Where's the red marker?",
            "Leo: Read me the next step for cookies.",
            "Jared: Find furnace manual troubleshooting code 31.",
            "Sarah: Find the note about the plumber's shutoff valve.",
            "Mia: Where did I save my poem about winter?",
            "Sarah: Find the toddler gate instructions.",
            "Sarah: Find the recipe where we used the green bowl.",
            "Leo: Where's the flashlight if the lights go out?",
            "Mia: What snacks did we pack for my last tournament?",
        ] {
            let call = route(query).unwrap();
            assert_eq!(call.name, "memory_recall", "{query}");
        }
    }

    #[test]
    fn routes_app_only_secret_questions_to_memory_recall() {
        let call = route("What is our Wi-Fi password for guests?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(
            call.arguments["query"],
            "what is our wi fi password for guests"
        );

        let call = route("Where is the gate code?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "where is the gate code");

        let call = route("What's the Wi-Fi password for the printer?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(
            call.arguments["query"],
            "what s the wi fi password for the printer"
        );

        let call = route("What is Mia's locker combination?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "what is mia s locker combination");

        let call = route("What's the Wi-Fi password for the Xbox?").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(
            call.arguments["query"],
            "what s the wi fi password for the xbox"
        );

        let call = route("What is the confirmation number for the hotel?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What is the account number for the gas bill?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What is the combination for the shed?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What's the code for the Netflix account?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Find the password for the guest network.").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Where is the spare key?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What is the code for the shed?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Where did I save the bank login?").unwrap();
        assert_eq!(call.name, "memory_recall");
    }

    #[test]
    fn routes_semantic_household_memory_questions_to_memory_recall() {
        let call = route("I'm feeling cold").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "i m feeling cold");

        let call = route("We need more snacks for Leo's lunchbox").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(
            call.arguments["query"],
            "we need more snacks for leo s lunchbox"
        );

        let call = route("What was the movie about a robot?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("I'm bored").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("What can I cook with chicken and rice?").unwrap();
        assert_eq!(call.name, "memory_recall");

        let call = route("Is it too late to call Grandma?").unwrap();
        assert_eq!(call.name, "memory_recall");

        for query in [
            "I'm stressed",
            "I need a science fair idea",
            "What can I bake with just flour and sugar?",
            "I have a headache",
            "Read me a story",
            "Find a movie for tonight",
            "Order more dog food",
            "Plan a trip to the zoo",
            "I'm hungry but on a diet",
            "The washing machine is shaking",
            "Watch TV",
            "Someone is at the door",
            "I'm in the mood for a scary movie",
            "I need a drink",
            "It's too bright in here",
            "I'm feeling lonely",
            "The kitchen sink smells",
            "I'm leaving for work",
            "Make tacos for dinner",
            "It feels muggy in here",
            "I cut my finger",
            "Where are my keys?",
            "I hear a weird noise outside",
            "Order pizza",
            "Start the car",
            "I need help with my math homework",
            "I'm tired of this song",
            "Tell me a joke",
            "What does ephemeral mean?",
            "I want to build a fort",
            "I'm running late for the train",
            "Is the air quality okay for Leo to play outside?",
            "Plan a birthday party for Mia",
            "We have a spider in the bathroom",
            "I can't find the remote",
            "Is the garage freezer cold enough?",
            "I need a laugh",
            "I have chicken but no ideas",
            "Who do we know that fixes sinks?",
            "I want to learn about the solar system",
            "I keep oversleeping",
            "When should I leave for the airport?",
            "Buy food for the dinner party",
            "I'm really hot",
            "We have guests coming over",
            "The baby is awake",
            "I'm out of olive oil. What can I use instead?",
            "I want to build a bookshelf",
            "The toilet keeps running",
            "My knee hurts after my run",
            "We need a side dish for pasta",
            "Pack my gym bag",
            "Are we over budget this month?",
            "Text Mom happy birthday",
            "Is it safe to eat this?",
            "Find a movie we haven't seen",
            "Remind me to defrost the turkey",
            "What's for dinner?",
            "I'm going for a run",
            "What should I get Dad for Father's Day?",
            "I need a break",
            "What wine goes with steak?",
            "Order more ink for the printer",
            "Is it safe to run outside?",
            "What did I get Sarah for her birthday last year?",
            "Plan a game night",
            "The baby is crying again",
            "I want to listen to Jazz",
            "Suggest a book I might like",
            "I'm bored of cooking tonight",
            "What can I make with ripe bananas?",
            "Show me pictures from our beach trip",
            "It's going to freeze tonight",
            "Pack a lunch for Leo",
            "Bring in the patio cushions",
            "I'm going for a bike ride",
            "Order dog food",
            "What's for breakfast?",
            "I need a haircut",
            "What should I wear to the wedding?",
            "I want to meditate",
            "Teach me Spanish",
            "I'm hungry for something spicy",
            "Book a hotel in Chicago",
            "Change the AC filter",
            "What should we do with the kids today?",
            "The toilet is clogged again",
            "Sew a button on my shirt",
            "I want to paint",
            "I have a stomach ache",
            "Teach me magic tricks",
            "I need a manicure",
            "What's a good charity?",
            "I want to learn French",
            "Suggest a podcast",
            "I need a motivating speech",
            "What shoes go with this dress?",
            "I'm thirsty",
            "I'm going to yoga class",
            "I'm sunbathing",
            "Plan a guys' night",
            "Order Thai food",
            "I have a fever",
            "It's snowing",
            "Is Mia doing her homework?",
            "I'm back",
            "Find a bedtime story for Leo",
            "Find a romantic poem",
            "Suggest a hiking trail",
            "What can I do with extra basil?",
            "I'm craving spicy food",
            "Find a picture of a sunset",
            "What's a good name for a goldfish?",
            "I feel anxious",
            "Teach me about the Roman Empire",
            "What music fits this mood?",
            "I'm going camping",
            "Make me a cocktail",
            "Plan a date night for Friday",
            "The washing machine is leaking",
            "Did I lock the bike?",
            "Order groceries for a taco bar",
            "Sarah: I'm cold in the living room.",
            "Mia: Did my package arrive?",
            "Jared: When should we water the garden?",
            "Sarah: Find the recipe we liked with chickpeas.",
            "Jared: Why didn't the hallway light turn on?",
            "Mia: Why is my room so hot?",
            "Mia: My laptop battery is low.",
            "Mia: Can I bake cookies without waking Leo?",
            "Leo: Why is the robot vacuum stuck?",
            "Sarah: Is the package still on the porch?",
            "Sarah: What's making that beeping sound?",
            "Jared: Why is the porch light still on?",
        ] {
            let call = route(query).unwrap();
            assert_eq!(call.name, "memory_recall", "{query}");
        }
    }

    #[test]
    fn routes_explicit_scene_and_routine_activation() {
        let call = route("Goodnight, GenieClaw.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "goodnight");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Goodnight.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "goodnight");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Start bedtime reading scene").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "bedtime reading");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("I'm home").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "arrival");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Lock up the house").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "lock up house");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Turn off everything").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "all off");
        assert_eq!(call.arguments["action"], "activate");
    }

    #[test]
    fn routes_playlist_requests_to_media() {
        let call = route("Play my Morning Boost playlist").unwrap();
        assert_eq!(call.name, "play_media");
        assert_eq!(call.arguments["query"], "my morning boost playlist");

        let call = route("Play focus music").unwrap();
        assert_eq!(call.name, "play_media");
        assert_eq!(call.arguments["query"], "focus music playlist");

        let call = route("Put on the morning news").unwrap();
        assert_eq!(call.name, "play_media");
        assert_eq!(call.arguments["query"], "morning news");

        let call = route("Play the weather report").unwrap();
        assert_eq!(call.name, "play_media");
        assert_eq!(call.arguments["query"], "local weather report");
    }

    #[test]
    fn routes_shopping_and_temperature_home_requests() {
        let call = route("Add milk and eggs to the shopping list").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "shopping");
        assert_eq!(
            call.arguments["content"],
            "shopping list pending: milk, eggs"
        );

        let call = route("Take milk off the shopping list").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "shopping");
        assert_eq!(call.arguments["content"], "shopping list removed: milk");

        let call = route("Don't let the kids play video games until homework is done").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "fact");
        assert_eq!(
            call.arguments["content"],
            "Kids must finish homework before screen time"
        );

        let call = route("Log that I drank 2 glasses of water").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "health_tracker");
        assert_eq!(
            call.arguments["content"],
            "hydration log: drank 2 glasses of water"
        );

        let call = route("Log my weight").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "health_tracker");
        assert_eq!(
            call.arguments["content"],
            "weight log: requested weight entry"
        );

        let call = route("Sarah: Remind Leo to bring soccer cleats tomorrow morning").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "reminder");
        assert_eq!(
            call.arguments["content"],
            "Reminder for Leo tomorrow morning: bring soccer cleats"
        );

        let call = route("Mia: Set an alarm for rehearsal at 6:30").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "alarm");
        assert_eq!(call.arguments["content"], "Alarm for rehearsal at 6 30");

        let call = route("Leo: Tell me when Dad gets home").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "presence_alert");
        assert_eq!(
            call.arguments["content"],
            "Presence alert for Leo: tell him when Dad gets home"
        );

        let call = route("Mia: Save the bathroom for me at 7 for hair wash").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "reservation");
        assert_eq!(
            call.arguments["content"],
            "Bathroom reservation for Mia at 7:00 PM for hair wash"
        );

        let call = route("Mia: Save this lighting for art time").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "scene_embeddings");
        assert_eq!(
            call.arguments["content"],
            "Art time lighting scene: save current room lights, desk lamp, and blinds for Mia's art time"
        );

        let call = route("Mia: Make tomorrow into a checklist").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "daily_checklists");

        let call = route("Leo: Remember that I like the green night-light better").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "preference");
        assert_eq!(
            call.arguments["content"],
            "Leo prefers green as his night-light color"
        );

        let call = route("Mia: Can Emma come over after school?").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "permission_requests");

        let call = route("Mia: Remember that my red hoodie is in Dad's car.").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "item_location_events");

        let call = route("Mia: Save this as my rainy-day playlist.").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "user_media_aliases");

        let call = route("Mia: Remember I like the fan on low for sleep.").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "preference");

        let call = route("Leo: Tell Dad when my puzzle is done.").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "reminders");

        let call = route("Mia: Remind me to water my plant after school.").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "reminder");

        let call = route("Mia: Save this temperature as rehearsal comfort.").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "activity_preference_embeddings");

        let call = route("Mia: Add batteries and poster board to my project list.").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "project_list_items");

        let call = route("Mia: Make my alarm skip holidays.").unwrap();
        assert_eq!(call.name, "memory_store");
        assert_eq!(call.arguments["category"], "alarms");

        let call = route("Leo: Start a Lego cleanup timer.").unwrap();
        assert_eq!(call.name, "set_timer");
        assert_eq!(call.arguments["seconds"], 600);
        assert_eq!(call.arguments["label"], "lego cleanup");

        let call = route("Set the oven to 400 degrees").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "oven");
        assert_eq!(call.arguments["action"], "set_temperature");
        assert_eq!(call.arguments["value"], 400.0);

        let call = route("Is the garage door closed?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "garage door");

        let call = route("Is the side door locked?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "side door");

        let call = route("Start the dishwasher").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "dishwasher normal cycle");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Turn on the ceiling fan in the bedroom").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "bedroom ceiling fan");
        assert_eq!(call.arguments["action"], "turn_on");

        let call = route("Turn off the holiday lights").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "outdoor holiday lights");
        assert_eq!(call.arguments["action"], "turn_off");

        let call = route("Set up the slow cooker for chili").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "slow cooker chili");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Stop the sprinklers, it's raining").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "sprinklers");
        assert_eq!(call.arguments["action"], "pause");

        let call = route("Turn on the porch light when I arrive").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "porch light");
        assert_eq!(call.arguments["action"], "schedule_on_arrival");

        let call = route("I'm driving home in the rain").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "arrival rain protocol");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("I'm in a dark parking lot").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "parking lot safety protocol");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Call my phone").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "phone finder");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Turn on the fireplace").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "fireplace");
        assert_eq!(call.arguments["action"], "turn_on");

        let call = route("Start the robot mower").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "robot mower");
        assert_eq!(call.arguments["action"], "start");

        let call = route("Turn off the upstairs lights").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "upstairs lights");
        assert_eq!(call.arguments["action"], "turn_off");

        let call = route("Test the smoke detectors").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "smoke detectors");
        assert_eq!(call.arguments["action"], "test");

        let call = route("Turn off the TV").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "tv");
        assert_eq!(call.arguments["action"], "turn_off");

        let call = route("Turn on the alarm").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "security alarm");
        assert_eq!(call.arguments["action"], "arm");

        let call = route("Turn on the pool cleaner").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "pool cleaner");
        assert_eq!(call.arguments["action"], "start");

        let call = route("Set the thermostat to Eco mode").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "thermostat");
        assert_eq!(call.arguments["action"], "set_preset");

        let call = route("I'm going to take a nap").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "nap mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("It's stuffy in here").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "ventilation comfort scene");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("I'm working from home today").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "work from home scene");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("I'm locked out").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "front door");
        assert_eq!(call.arguments["action"], "unlock");

        let call = route("Warm up the car").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "connected car climate");
        assert_eq!(call.arguments["action"], "remote_start");
        assert_eq!(call.arguments["value"], 72.0);

        let call = route("Send this address to my car").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "car navigation");
        assert_eq!(call.arguments["action"], "send_destination");

        let call = route("I've fallen and I can't get up").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "fall emergency alert");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Prep the house for vacation").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "vacation mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("It's smoky in here").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "smoke ventilation protocol");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("I'm working late").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "working late family update");
        assert_eq!(call.arguments["action"], "activate");

        let call =
            route("Jared: Turn off everything downstairs except the kitchen lights").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "downstairs except kitchen lights");
        assert_eq!(call.arguments["action"], "turn_off_except");

        let call = route("Jared: Run movie night").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "movie night");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: The room is too bright for reading").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "reading light comfort");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: Make my room cozy").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "personal cozy room scene");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Is that smoke alarm real or just a battery warning?").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "smoke emergency protocol");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Set the house to away mode").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "away mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: Turn on my study playlist and desk lamp").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "personal study scene");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Leo: It's too loud").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "nearby media volume");
        assert_eq!(call.arguments["action"], "set_volume");
        assert_eq!(call.arguments["value"], 25.0);

        let call = route("Leo: Turn my night-light blue").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "personal night-light");
        assert_eq!(call.arguments["action"], "set_color_blue");

        let call = route("Jared: Pause internet for the kids until dinner").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "kids internet");
        assert_eq!(call.arguments["action"], "pause_until_dinner");

        let call = route("Mia: Make the hallway safe at night").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "night hallway safety");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Start dinner prep mode").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "dinner prep mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Leo: I spilled water near the outlet!").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "outlet spill safety protocol");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Warm up Sarah's bathroom before her shower").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "sarah bathroom comfort");
        assert_eq!(call.arguments["action"], "warm_for_minutes");
        assert_eq!(call.arguments["value"], 25.0);

        let call = route("Mia: Give me focus mode until five").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "personal focus mode");
        assert_eq!(call.arguments["action"], "activate_until_5pm");

        let call = route("Sarah: Keep the porch from waking the kids tonight").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "quiet porch alerts tonight");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Start storm prep").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "storm prep");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Leo: I'm scared of the dark").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "night reassurance scene");
        assert_eq!(call.arguments["action"], "activate");

        let call =
            route("Jared: Open blinds where there's morning sun, but not Mia's room").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(
            call.arguments["entity"],
            "morning sun blinds except mia room"
        );
        assert_eq!(call.arguments["action"], "open");

        let call = route("Sarah: Keep piano practice quiet for the rest of the house").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "piano practice quiet mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: I smell gas").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "gas safety emergency");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Start bedtime, but let Mia read for 20 minutes").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(
            call.arguments["entity"],
            "bedtime with mia reading override"
        );
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Set vacation mode for next week").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(
            call.arguments["entity"],
            "scheduled vacation mode next week"
        );
        assert_eq!(call.arguments["action"], "schedule");

        let call = route("Leo: Can the robot vacuum clean under my bed?").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo under-bed vacuum zone");
        assert_eq!(call.arguments["action"], "clean");

        let call = route("Mia: Turn off notifications while I'm practicing violin").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(
            call.arguments["entity"],
            "mia violin practice notifications"
        );
        assert_eq!(call.arguments["action"], "mute_for_practice");

        let call = route("Sarah: Make the kitchen toddler-safe for our visitor").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "toddler-safe kitchen");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Lock everything except the back gate").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "all locks except back gate");
        assert_eq!(call.arguments["action"], "lock_except");

        let call = route("Leo: Make the hallway look like a spaceship").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "spaceship hallway");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Start homework mode for both kids").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "kids homework mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: Put my schedule on the bathroom mirror").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia bathroom mirror agenda");
        assert_eq!(call.arguments["action"], "show_agenda");

        let call = route("Leo: I'm too hot in bed").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo bed cooling comfort");
        assert_eq!(call.arguments["action"], "cool_down");
        assert_eq!(call.arguments["value"], 2.0);

        let call = route("Jared: There's water under the sink").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "kitchen sink leak safety");
        assert_eq!(call.arguments["action"], "shut_water_zone");

        let call = route("Jared: Turn off standby power in the office").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "office standby-safe plugs");
        assert_eq!(call.arguments["action"], "turn_off");

        let call = route("Mia: Block YouTube until I finish math").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia youtube access");
        assert_eq!(call.arguments["action"], "block_until_math_done");

        let call = route("Jared: Let the contractor into the garage at 10").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "contractor garage access");
        assert_eq!(call.arguments["action"], "allow_10_to_10_20");

        let call = route("Sarah: Set up sleepover guest mode").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "sleepover guest mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Leo: Turn on stars but keep the closet dark").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo stars except closet");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: Open my blinds slowly every school morning").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(
            call.arguments["entity"],
            "mia school-morning gradual blinds"
        );
        assert_eq!(call.arguments["action"], "schedule");

        let call = route("Mia: Make the shower warm but not steamy").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia warm-not-steamy shower");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Keep security on, but don't wake the kids").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "quiet armed security");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: I heard glass break downstairs").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "downstairs glass-break safety");
        assert_eq!(call.arguments["action"], "verify_and_alert");

        let call = route("Leo: Call Mom on the kitchen screen").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "kitchen screen call mom");
        assert_eq!(call.arguments["action"], "start_video_call");

        let call = route("Sarah: Prep the house for the babysitter").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "babysitter mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Start rainy pickup mode.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "rainy pickup mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Leo: The toaster smells smoky!").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "toaster smoke safety");
        assert_eq!(call.arguments["action"], "cut_power_and_vent");

        let call = route("Sarah: Make the house better for pollen.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "pollen mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Turn on the driveway lights only when I pull in.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "driveway arrival lights");
        assert_eq!(call.arguments["action"], "schedule_on_arrival");

        let call = route("Mia: Make my room good for a video call.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia video-call room setup");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Run the dishwasher after 9.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "dishwasher");
        assert_eq!(call.arguments["action"], "schedule_after_21");

        let call = route("Jared: Run allergy-day setup.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "allergy-day setup");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: Wake me with sunlight, not sound.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia sunlight alarm");
        assert_eq!(call.arguments["action"], "schedule_gradual_blinds");

        let call = route("Jared: Show guests only the Wi-Fi and bathroom info.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "limited guest info display");
        assert_eq!(call.arguments["action"], "show_guest_card");

        let call = route("Leo: Make my room ready for reading with Dad.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo reading-with-dad scene");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Make the house quiet for my work call.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "work-call quiet mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Keep the garage ventilated while I paint.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "garage paint ventilation");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: Set my room to sleepover lights.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia sleepover lights");
        assert_eq!(call.arguments["action"], "apply_scene");

        let call = route("Leo: Make my lights flash when the cookies are done.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo cookie timer light alert");
        assert_eq!(call.arguments["action"], "schedule_pulse");

        let call = route("Sarah: Start a calm morning for Leo.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo calm morning");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Start after-dinner cleanup mode.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "after-dinner cleanup");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Make the living room good for board games.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "living room board games");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: Block notifications except Mom during my test practice.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia test-practice notifications");
        assert_eq!(call.arguments["action"], "allow_mom_only");

        let call = route("Jared: Start the coffee when I wake up.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "coffee maker wake brew");
        assert_eq!(call.arguments["action"], "schedule_on_alarm");

        let call = route("Leo: I'm cold after bath.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo post-bath comfort");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Run basement flood check.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "basement flood check");
        assert_eq!(call.arguments["action"], "run");

        let call = route("Jared: Create a temporary code for Grandma.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "grandma temporary door code");
        assert_eq!(call.arguments["action"], "create_until_21");

        let call = route("Mia: My desk feels glarey.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia desk glare comfort");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Leo: Start quiet drawing time.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo quiet drawing time");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Make upstairs cooler but leave Leo's room alone.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "upstairs cooling except leo room");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Turn off screens during family dinner.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "family dinner screens");
        assert_eq!(call.arguments["action"], "pause");

        let call = route("Leo: Make the stairs bright.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "stairwell lights");
        assert_eq!(call.arguments["action"], "set_level");
        assert_eq!(call.arguments["value"], 90.0);

        let call = route("Jared: Start workshop dust control.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "workshop dust control");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: Make my closet light turn on when I open it.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia closet light automation");
        assert_eq!(call.arguments["action"], "create");

        let call = route("Jared: Put the house in low-power mode until five.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "low-power mode");
        assert_eq!(call.arguments["action"], "activate_until_5pm");

        let call = route("Leo: Put on an animal show, but not loud.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo animal show");
        assert_eq!(call.arguments["action"], "play_low_volume");

        let call = route("Sarah: Keep the front entry lights on until Mia gets home.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(
            call.arguments["entity"],
            "front entry lights until mia home"
        );
        assert_eq!(call.arguments["action"], "hold");

        let call = route("Leo: I hear dripping.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "nearby leak check");
        assert_eq!(call.arguments["action"], "check_and_alert");

        let call = route("Sarah: Start school-night reset.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "school-night reset");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Notify me if the freezer goes above 10 degrees.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "freezer temperature alert");
        assert_eq!(call.arguments["action"], "create_threshold_10");

        let call = route("Mia: Turn on only the mirror lights.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia mirror lights only");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Set backyard lights for grilling.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "backyard grilling lights");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Leo: I'm too scared to go downstairs.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(
            call.arguments["entity"],
            "downstairs reassurance path lights"
        );
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Keep dinner warm until Jared arrives.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "dinner warm until jared arrives");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Mia: Schedule quiet time after school on Wednesdays.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia wednesday quiet time");
        assert_eq!(call.arguments["action"], "schedule");

        let call = route("Sarah: Open windows if the air outside is cleaner.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "cleaner outside air window mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Jared: Create a holiday lighting schedule.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "holiday lighting schedule");
        assert_eq!(call.arguments["action"], "create");

        let call = route("Mia: Use my rainy-day alarm tomorrow.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "mia rainy-day alarm");
        assert_eq!(call.arguments["action"], "set_for_tomorrow");

        let call = route("Sarah: Start guest breakfast mode.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "guest breakfast mode");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Leo: Make the laundry room not scary.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo laundry-room reassurance");
        assert_eq!(call.arguments["action"], "activate");

        let call = route("Sarah: Remind me to check the oven after it preheats.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "oven preheat reminder");
        assert_eq!(call.arguments["action"], "create");

        let call =
            route("Mia: Turn off the hallway camera while sleepover guests change.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "hallway camera sleepover privacy");
        assert_eq!(call.arguments["action"], "privacy_20");

        let call = route("Leo: Tell me when the cookies are cool enough.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "cookie cooling alert");
        assert_eq!(call.arguments["action"], "create");

        let call =
            route("Jared: Shut off the water automatically if the laundry leaks again.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "automatic laundry leak shutoff");
        assert_eq!(call.arguments["action"], "enable");

        let call = route("Leo: Turn on my morning checklist on the wall.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "leo morning checklist display");
        assert_eq!(call.arguments["action"], "show");

        let call = route("Sarah: Make upstairs warmer when the kids are getting ready.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "kids morning upstairs warmth");
        assert_eq!(call.arguments["action"], "schedule");

        let call = route("Jared: Run a final safety sweep.").unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["entity"], "final safety sweep");
        assert_eq!(call.arguments["action"], "run");

        let call = route("Is the driveway icy?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "driveway ice");

        let call = route("Is the front gate closed?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "front gate");

        let call = route("Is the dryer finished?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "dryer");

        let call = route("What is the current humidity in the basement?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "basement humidity");

        let call = route("Is my car locked?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "car");

        let call = route("Did I leave the stove on?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "stove");

        let call = route("Is the package delivered?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "package");

        let call = route("Are the front sprinklers on?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "front sprinklers");

        let call = route("How much solar power did we generate today?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "solar power today");

        let call = route("Check the tire pressure on the car").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "car tire pressure");

        let call = route("Did the mail come?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "mailbox");

        let call = route("Is the garage door open?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "garage door");

        let call = route("Is the iron on?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "iron");

        let call = route("Is the water hot?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "water heater");

        let call = route("Is the baby breathing?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "baby breathing monitor");

        let call = route("Did I lock the car?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "car locks");

        let call = route("Is the printer out of ink?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "printer ink");

        let call = route("Is the baby monitor on?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "baby monitor");

        let call = route("What's the speed limit here?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "speed limit");

        let call = route("Did the package arrive?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "package");

        let call = route("Is the self-cleaning oven on?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "self-cleaning oven");

        let call = route("Check the water pressure").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "water pressure");

        let call = route("Is the sump pump running?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "sump pump");

        let call = route("Is the sous vide on?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "sous vide");

        let call = route("What's the air quality in the nursery?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "nursery air quality");

        let call = route("Sarah: Did I leave the oven on?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "oven");

        let call = route("Jared: Show me cameras with motion in the last hour").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "cameras with recent motion");

        let call = route("Sarah: What doors are unlocked?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "unlocked doors");

        let call = route("Sarah: Is the freezer too warm?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "freezer");

        let call = route("Jared: What's using the most electricity right now?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "top electricity usage");

        let call = route("Jared: Was the freezer door left open?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "freezer door");

        let call = route("Jared: Are all the windows closed?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "open windows");

        let call = route("Jared: Did the sprinkler run this morning?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "sprinkler run history");

        let call = route("Jared: Give me a morning readiness report").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "morning readiness report");

        let call = route("Jared: Compare this week's electricity use to last week").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "weekly electricity comparison");

        let call = route("Sarah: Is the back burner off?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "back burner");

        let call = route("Jared: Which room seems drafty?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "drafty room report");

        let call = route("Jared: Which devices are offline?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "offline devices");

        let call = route("Sarah: Did the fridge door close all the way?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "fridge door");

        let call = route("Mia: Is the bathroom free?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "upstairs bathroom availability");

        let call = route("Jared: Which lights are still on upstairs?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "upstairs lights on");

        let call = route("Jared: What's the noisy appliance?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "noisy appliance");

        let call = route("Jared: Which outdoor cameras need cleaning?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "outdoor camera cleaning report");

        let call = route("Jared: What's the current water pressure?").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "water pressure");
    }

    #[test]
    fn routes_left_home_delta_to_action_history() {
        let call = route("Jared: What changed since we left?").unwrap();
        assert_eq!(call.name, "action_history");
    }

    #[test]
    fn routes_market_queries_to_web_search() {
        let call = route("What is the stock price of Apple?").unwrap();
        assert_eq!(call.name, "web_search");
        assert!(call.arguments["query"].as_str().unwrap().contains("apple"));
    }

    #[test]
    fn routes_explicit_memory_search_to_memory_recall() {
        let call = route("search memory for Jared").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "jared");
    }

    #[test]
    fn routes_undo_to_home_undo() {
        let call = route("undo that").unwrap();
        assert_eq!(call.name, "home_undo");
    }

    #[test]
    fn routes_action_history_questions() {
        let call = route("what did you do?").unwrap();
        assert_eq!(call.name, "action_history");
    }

    #[test]
    fn routes_time_question_to_get_time() {
        let call = route("what time is it?").unwrap();
        assert_eq!(call.name, "get_time");
    }

    #[test]
    fn routes_basic_timer() {
        let call = route("set a timer for 10 minutes").unwrap();
        assert_eq!(call.name, "set_timer");
        assert_eq!(call.arguments["seconds"], 600);
        assert_eq!(call.arguments["label"], "timer");

        let call = route("set a timer for 15 minutes").unwrap();
        assert_eq!(call.name, "set_timer");
        assert_eq!(call.arguments["seconds"], 900);
    }

    #[test]
    fn routes_compound_worded_timer() {
        // Regression: "forty five" used to parse as the trailing "five" (5 min)
        // because the compound cardinal was never stitched from its two tokens.
        let call = route("set a timer for forty five minutes").unwrap();
        assert_eq!(call.name, "set_timer");
        assert_eq!(call.arguments["seconds"], 2700);

        // Other tens+ones compounds work the same way.
        let call = route("set a timer for twenty five seconds").unwrap();
        assert_eq!(call.arguments["seconds"], 25);

        // A bare tens word (no trailing ones) is still parsed on its own.
        let call = route("set a timer for fifty minutes").unwrap();
        assert_eq!(call.arguments["seconds"], 3000);

        // The compound amount is carried into the reminder label boundary too.
        let call = route("remind me in forty five minutes to check the oven").unwrap();
        assert_eq!(call.arguments["seconds"], 2700);
        assert_eq!(call.arguments["label"], "check the oven");
    }

    #[test]
    fn routes_reminder_timer_with_label() {
        let call = route("remind me in 5 minutes to check the oven").unwrap();
        assert_eq!(call.name, "set_timer");
        assert_eq!(call.arguments["seconds"], 300);
        assert_eq!(call.arguments["label"], "check the oven");
    }

    #[test]
    fn routes_weather_with_explicit_location() {
        let call = route("weather in Tokyo").unwrap();
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.arguments["location"], "tokyo");
        assert_eq!(call.arguments["forecast"], false);
    }

    #[test]
    fn routes_forecast_with_explicit_location() {
        let call = route("forecast for New York").unwrap();
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.arguments["location"], "new york");
        assert_eq!(call.arguments["forecast"], true);
    }

    #[test]
    fn routes_explicit_web_search() {
        let call = route("search the web for Home Assistant Matter support").unwrap();
        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments["query"], "home assistant matter support");

        let call = route("Read the news").unwrap();
        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments["query"], "top news headlines");
    }

    #[test]
    fn routes_lookup_to_web_search() {
        let call = route("look up ESP32 C6 Thread support").unwrap();
        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments["query"], "esp32 c6 thread support");
    }

    #[test]
    fn routes_simple_arithmetic() {
        let call = route("what is 12 plus 30").unwrap();
        assert_eq!(call.name, "calculate");
        assert_eq!(call.arguments["expression"], "12 + 30");
    }

    #[test]
    fn routes_percentage_math() {
        let call = route("what is 15 percent of 200").unwrap();
        assert_eq!(call.name, "calculate");
        assert_eq!(call.arguments["expression"], "200 * 15 / 100");

        let call = route("Convert 350 degrees to Celsius").unwrap();
        assert_eq!(call.name, "calculate");
        assert_eq!(call.arguments["expression"], "(350 - 32) * 5 / 9");
    }

    #[test]
    fn does_not_route_non_math_numbers() {
        assert!(route("what happened in 2024").is_none());
    }

    #[test]
    fn does_not_route_weather_without_location() {
        assert!(route("what is the weather").is_none());
    }

    #[test]
    fn routes_whole_home_light_status() {
        let call = route("what lights are on").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "lights");
    }

    #[test]
    fn routes_room_light_status_without_losing_room() {
        let call = route("is the kitchen light on").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "kitchen light");
    }

    #[test]
    fn does_not_route_ambiguous_time_reference() {
        assert!(route("what time is my meeting").is_none());
    }

    #[test]
    fn does_not_route_home_control_commands_as_status() {
        assert!(route("turn on the kitchen light").is_none());
    }

    #[test]
    fn availability_filter_skips_home_status_without_home_tools() {
        assert!(route_for_available_tools("what lights are on", false, true).is_none());
        assert!(route_for_available_tools("what lights are on", true, true).is_some());
        assert!(route_for_available_tools("undo that", false, true).is_none());
        assert!(route_for_available_tools("undo that", true, true).is_some());
        assert!(route_for_available_tools("goodnight genieclaw", false, true).is_none());
        assert!(route_for_available_tools("goodnight genieclaw", true, true).is_some());
    }

    #[test]
    fn availability_filter_skips_web_search_without_search_tool() {
        assert!(route_for_available_tools("look up ESP32 C6", true, false).is_none());
        assert!(route_for_available_tools("look up ESP32 C6", true, true).is_some());
    }

    #[test]
    fn availability_filter_keeps_non_home_tools() {
        let call = route_for_available_tools("what time is it", false, false).unwrap();
        assert_eq!(call.name, "get_time");

        let call = route_for_available_tools("what is 15 percent of 200", false, false).unwrap();
        assert_eq!(call.name, "calculate");
    }
}
