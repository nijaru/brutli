// The static dictionary search is reference-derived and contains a frozen generated table.
// Keep that token stream outside rustfmt while still compiling and linting it normally.
include!("static_dictionary.in");
