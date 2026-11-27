export type Greeting = string;

enum Punctuation {
  Bang = "!",
}

export function greet(name: string): Greeting {
  return `hello ${name}${Punctuation.Bang}`;
}
