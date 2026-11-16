//! The initials an avatar shows in place of a picture, one rule for every view, as Teams makes them: the text in
//! brackets goes ("Ann Lee (Guest)"), each word keeps only its letters and digits (quotes, dots, dashes and emoji
//! go), then a person's initials are the first letters of the first and the last word, a team's those of its first
//! two words. A person's name with no letter (a phone number) or in a script Teams draws no initials for (Chinese,
//! Japanese, Korean, Arabic) has none: the avatar shows its blank face.

/// A person's initials: the first letters of the first and the last word of `name`, one for a name of one word,
/// none when Teams draws none.
#[must_use]
pub fn initials(name: &str) -> String {
    let words = person_words(name);
    match words.as_slice() {
        [] => String::new(),
        [only] => upper(only),
        [first, .., last] => upper(first) + &upper(last),
    }
}

/// A person's first initial alone (half of a group chat's split avatar, a workflow owner's face).
#[must_use]
pub fn first_initial(name: &str) -> String {
    person_words(name).first().map(|word| upper(word)).unwrap_or_default()
}

/// A team's (or an app's) initials: the first letters of its first two words.
#[must_use]
pub fn team_initials(name: &str) -> String {
    words(name).iter().take(2).map(|word| upper(word)).collect()
}

/// `initials` as the kit's avatar is handed them: the kit makes its own of the name it gets, which for one word of
/// two letters keeps only the first unless the letters are apart.
#[must_use]
pub(crate) fn kit_name(initials: &str) -> String {
    let letters: Vec<String> = initials.chars().map(String::from).collect();
    letters.join(" ")
}

/// The words of `name` a person's initials come from: none when no word has a letter or a word is in a script
/// without initials.
fn person_words(name: &str) -> Vec<String> {
    let words = words(name);
    let lettered = words.iter().any(|word| word.chars().any(char::is_alphabetic));
    if !lettered || words.iter().any(|word| word.chars().any(without_initials)) {
        return Vec::new();
    }
    words
}

/// The words of `name` initials come from: the text in brackets dropped, letters and digits kept.
fn words(name: &str) -> Vec<String> {
    without_brackets(name)
        .split_whitespace()
        .map(|word| word.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
        .filter(|word| !word.is_empty())
        .collect()
}

/// `name` without the text from each opening bracket to the next closing one.
fn without_brackets(name: &str) -> String {
    let mut kept = String::with_capacity(name.len());
    let mut inside = false;
    for c in name.chars() {
        match c {
            '(' | '[' | '{' | '<' => inside = true,
            ')' | ']' | '}' | '>' if inside => inside = false,
            _ if !inside => kept.push(c),
            _ => {}
        }
    }
    kept
}

/// Whether `c` is in a script Teams draws no initials for: Arabic, Korean, Japanese kana, Chinese.
fn without_initials(c: char) -> bool {
    matches!(
        u32::from(c),
        0x0600..=0x06FF
            | 0x0750..=0x077F
            | 0x08A0..=0x08FF
            | 0x1100..=0x11FF
            | 0x3040..=0x30FF
            | 0x3130..=0x318F
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7FF
            | 0xF900..=0xFAFF
            | 0x2_0000..=0x2_FA1F
    )
}

/// The first letter of `word`, upper case.
fn upper(word: &str) -> String {
    word.chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_s_initials_are_the_first_letters_of_the_first_and_last_names() {
        assert_eq!(initials("Cora 'Cee' Nuñez"), "CN");
        assert_eq!(initials("Cora \"Cee\" Nuñez"), "CN");
        assert_eq!(initials("Ann Lee"), "AL");
        assert_eq!(initials("ann lee"), "AL");
        assert_eq!(initials("Mary Ann van Dyke"), "MD");
        assert_eq!(initials("  Ann   Lee  "), "AL");
    }

    #[test]
    fn brackets_and_their_text_give_no_initial() {
        assert_eq!(initials("Ann Lee (Guest)"), "AL");
        assert_eq!(initials("Ann Lee [External]"), "AL");
        assert_eq!(initials("Ann {Ops} Lee"), "AL");
        assert_eq!(initials("Ann Lee <Contoso>"), "AL");
        assert_eq!(initials("(Guest) Ann Lee"), "AL");
    }

    #[test]
    fn punctuation_and_emoji_give_no_initial() {
        assert_eq!(initials("Jean-Luc O'Brien"), "JO");
        assert_eq!(initials("🎉 Ann Lee 🎉"), "AL");
        assert_eq!(initials("«Ann» & Lee"), "AL");
        assert_eq!(initials("Q4 ✨ «planning» & co"), "QC");
        assert_eq!(initials("'Ann'"), "A");
    }

    #[test]
    fn a_name_of_one_word_gives_one_initial() {
        assert_eq!(initials("Cora"), "C");
        assert_eq!(initials("You"), "Y");
        assert_eq!(initials("bot-c@lab.example.com"), "B");
    }

    #[test]
    fn letters_of_other_scripts_are_initials_in_their_own_script() {
        assert_eq!(initials("Иван Петров"), "ИП");
        assert_eq!(initials("Ζωή Αλεξίου"), "ΖΑ");
        assert_eq!(initials("Łukasz Żak"), "ŁŻ");
        assert_eq!(initials("élodie dupont"), "ÉD");
    }

    #[test]
    fn a_name_teams_draws_no_initials_for_has_none() {
        assert_eq!(initials(""), "");
        assert_eq!(initials("   "), "");
        assert_eq!(initials("🎉✨"), "");
        assert_eq!(initials("(Guest)"), "");
        assert_eq!(initials("+1 425 555 0100"), "");
        assert_eq!(initials("王小明"), "");
        assert_eq!(initials("김 민준"), "");
        assert_eq!(initials("محمد علي"), "");
        assert_eq!(initials("Ann 王"), "");
    }

    #[test]
    fn a_first_initial_is_the_first_letter_of_the_first_name() {
        assert_eq!(first_initial("Cora 'Cee' Nuñez"), "C");
        assert_eq!(first_initial("'Ann' Lee"), "A");
        assert_eq!(first_initial("(Guest) Lee"), "L");
        assert_eq!(first_initial(""), "");
    }

    #[test]
    fn a_team_s_initials_are_the_first_letters_of_its_first_two_words() {
        assert_eq!(team_initials("Lab Team Alpha"), "LT");
        assert_eq!(team_initials("«Launch» — renamed live ✨"), "LR");
        assert_eq!(team_initials("Ops"), "O");
        assert_eq!(team_initials("✨"), "");
        assert_eq!(team_initials("开发 团队"), "开团");
    }

    #[test]
    fn the_kit_is_handed_each_initial_apart() {
        assert_eq!(kit_name("CN"), "C N");
        assert_eq!(kit_name("ИП"), "И П");
        assert_eq!(kit_name("C"), "C");
        assert_eq!(kit_name(""), "");
    }
}
