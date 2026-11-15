//! The name an identity gets when its owner leaves the name empty.

const VERBS: [&str; 24] = [
    "Leaping",
    "Running",
    "Jumping",
    "Hopping",
    "Flying",
    "Swimming",
    "Dancing",
    "Climbing",
    "Racing",
    "Rolling",
    "Sleeping",
    "Singing",
    "Diving",
    "Sliding",
    "Spinning",
    "Skipping",
    "Roaming",
    "Wandering",
    "Galloping",
    "Soaring",
    "Dashing",
    "Bouncing",
    "Floating",
    "Walking",
];

const ANIMALS: [&str; 24] = [
    "Frog", "Lion", "Elk", "Rooster", "Tiger", "Bear", "Fox", "Wolf", "Rabbit", "Eagle", "Owl",
    "Deer", "Horse", "Dolphin", "Penguin", "Turtle", "Panda", "Koala", "Zebra", "Giraffe",
    "Elephant", "Otter", "Duck", "Monkey",
];

/// "Verb Animal", picked from the identity's LXMF address so every path that names it agrees.
pub fn default_display_name(lxmf_hex: &str) -> String {
    let byte = |at: usize| {
        lxmf_hex
            .get(at..at + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            .map_or(0, usize::from)
    };
    format!(
        "{} {}",
        VERBS[byte(0) % VERBS.len()],
        ANIMALS[byte(2) % ANIMALS.len()]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_a_verb_and_an_animal_from_the_address() {
        assert_eq!(
            default_display_name("00000000000000000000000000000000"),
            "Leaping Frog"
        );
        assert_eq!(
            default_display_name("0101ffffffffffffffffffffffffffff"),
            "Running Lion"
        );
        assert_eq!(
            default_display_name("0101ffff"),
            default_display_name("0101ffff")
        );
        assert_eq!(default_display_name(""), "Leaping Frog");
        assert_eq!(default_display_name("zz"), "Leaping Frog");
        for hex in ["17ab", "ff00", "8c3e"] {
            let name = default_display_name(hex);
            let words: Vec<&str> = name.split(' ').collect();
            assert_eq!(words.len(), 2, "{name}");
            assert!(
                VERBS.contains(&words[0]) && ANIMALS.contains(&words[1]),
                "{name}"
            );
        }
    }

    #[test]
    fn the_lists_have_no_repeats() {
        assert_eq!(VERBS.iter().collect::<HashSet<_>>().len(), VERBS.len());
        assert_eq!(ANIMALS.iter().collect::<HashSet<_>>().len(), ANIMALS.len());
    }
}
